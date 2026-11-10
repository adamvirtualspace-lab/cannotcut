//! The viewer's GPU path: a picture's YUV planes go to the GPU as the decoder produced them and a
//! fragment shader turns them into RGB, so no pixel is converted on the CPU. The pictures at the
//! playhead are drawn one over another, each placed in the frame by its clip's transform.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cannotcut_media::{Colors, Matrix};
use cannotcut_timeline::{Framing, Reveal};
use iced::advanced::widget::Tree;
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer};
use iced::widget::shader::{self, Viewport};
use iced::{Element, Event, Length, Rectangle, Size, mouse, wgpu};

/// A 4x4 matrix and four vectors of four; see `Uniforms` in the shader.
const UNIFORM_FLOATS: usize = 32;
const UNIFORM_BYTES: usize = UNIFORM_FLOATS * size_of::<f32>();
/// The value 8-bit chroma samples take for "no colour", as sampled from a texture.
const NEUTRAL_CHROMA: f32 = 128.0 / 255.0;

/// A picture as 4:2:0 planes, drawn by the [`shader`](iced::widget::shader()) widget. Cheap to
/// clone.
#[derive(Clone)]
pub struct YuvPicture(Arc<Planes>);

struct Planes {
    /// Tells pictures apart, so each one is uploaded once.
    serial: u64,
    width: u32,
    height: u32,
    luma: Vec<u8>,
    /// A (Cb, Cr) pair per sample, half as wide and high as `luma`.
    chroma: Vec<u8>,
    /// How much of each pixel is seen, as big as `luma`; None when all of it is.
    alpha: Option<Vec<u8>>,
    colors: Colors,
}

impl YuvPicture {
    pub fn new(width: u32, height: u32, luma: Vec<u8>, chroma: Vec<u8>, colors: Colors) -> Self {
        Self::with_alpha(width, height, luma, chroma, None, colors)
    }

    /// A picture parts of which are see-through, by how much in `alpha`: a byte per pixel.
    pub fn with_alpha(
        width: u32,
        height: u32,
        luma: Vec<u8>,
        chroma: Vec<u8>,
        alpha: Option<Vec<u8>>,
        colors: Colors,
    ) -> Self {
        static NEXT_SERIAL: AtomicU64 = AtomicU64::new(0);
        Self(Arc::new(Planes {
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            luma,
            chroma,
            alpha,
            colors,
        }))
    }

    /// Its size in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.0.width, self.0.height)
    }
}

impl fmt::Debug for YuvPicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let planes = &self.0;
        write!(
            f,
            "YuvPicture #{} ({}x{})",
            planes.serial, planes.width, planes.height
        )
    }
}

/// The pictures at the playhead, the bottom one first, each with where it sits in a frame of
/// `frame` pixels' shape. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Composite {
    pub frame: (u32, u32),
    pub layers: Vec<(YuvPicture, Framing, Reveal)>,
}

impl<Message> shader::Program<Message> for Composite {
    type State = ();
    type Primitive = Self;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> Self {
        self.clone()
    }
}

impl shader::Primitive for Composite {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let frame_share = covered(self.frame, bounds.size());
        for (slot, (picture, framing, reveal)) in self.layers.iter().enumerate() {
            let corners = framing.corners(self.frame, (picture.0.width, picture.0.height)).0;
            let placed = Placed {
                frame_share,
                corners,
                opacity: framing.opacity() * reveal.fade,
                // Without a wipe, every point of the frame passes.
                wipe: reveal.wipe.map_or([0.0, 0.0, 1.0], |wipe| {
                    [wipe.direction[0], wipe.direction[1], wipe.threshold]
                }),
            };
            pipeline.load(device, queue, slot, &picture.0, placed);
        }
        pipeline.drawn = self.layers.len();
        // Pictures moved out of the frame are cut off at its edges, as they are in a film.
        let scale = viewport.scale_factor() as f32;
        let (width, height) = (
            bounds.width * frame_share[0] * scale,
            bounds.height * frame_share[1] * scale,
        );
        let left = bounds.x * scale + (bounds.width * scale - width) / 2.0;
        let top = bounds.y * scale + (bounds.height * scale - height) / 2.0;
        let target = viewport.physical_size();
        let x = left.max(0.0).floor() as u32;
        let y = top.max(0.0).floor() as u32;
        let right = ((left + width).ceil() as u32).min(target.width);
        let bottom = ((top + height).ceil() as u32).min(target.height);
        pipeline.frame_area = (x < right && y < bottom).then_some((x, y, right - x, bottom - y));
    }

    fn draw(&self, pipeline: &Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some((x, y, width, height)) = pipeline.frame_area else {
            return true;
        };
        render_pass.set_scissor_rect(x, y, width, height);
        render_pass.set_pipeline(&pipeline.render);
        for slot in pipeline.slots.iter().take(pipeline.drawn) {
            render_pass.set_bind_group(0, &slot.bind_group, &[]);
            render_pass.draw(0..4, 0..1);
        }
        true
    }
}

/// Where a layer goes, worked out on the CPU for the shader.
struct Placed {
    /// The share of the viewer the frame covers once fitted inside it.
    frame_share: [f32; 2],
    /// Where the picture's corners go: a 2x2 matrix, column by column, and an offset, taking a
    /// corner from -1 to 1 across the picture to -1 to 1 across the frame (y up).
    corners: [f32; 6],
    opacity: f32,
    /// A wipe's direction and threshold (see `Wipe`).
    wipe: [f32; 3],
}

/// GPU state of the viewer. iced keeps one for all pictures, which is right while there is a
/// single viewer; a second one would need textures of its own.
pub struct Pipeline {
    render: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The window's surface stores linear light, so the gamma that video RGB carries has to come
    /// off before writing to it.
    linear_target: bool,
    /// One for each layer drawn so far: made for the first picture in its place, and again
    /// when the picture size there changes.
    slots: Vec<Slot>,
    /// How many layers the latest composite has.
    drawn: usize,
    /// The frame's area on the target, in physical pixels: x, y, width, height.
    frame_area: Option<(u32, u32, u32, u32)>,
}

struct Slot {
    size: (u32, u32),
    luma: wgpu::Texture,
    chroma: wgpu::Texture,
    /// The picture's size when it has see-through parts, else a single texel, always white.
    alpha: wgpu::Texture,
    see_through: bool,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Serial of the picture the textures hold.
    holds: Option<u64>,
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cannotcut viewer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture(1),
                texture(2),
                texture(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cannotcut viewer"),
            source: wgpu::ShaderSource::Wgsl(include_str!("viewer.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cannotcut viewer"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let render = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cannotcut viewer"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fragment"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // A layer seen through shows what is under it.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cannotcut viewer"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        Self {
            render,
            layout,
            sampler,
            linear_target: format.is_srgb(),
            slots: Vec::new(),
            drawn: 0,
            frame_area: None,
        }
    }
}

impl Pipeline {
    /// Gets `picture` onto the GPU in layer `slot`, unless it is there already, and places it.
    fn load(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: usize,
        picture: &Planes,
        placed: Placed,
    ) {
        let size = (picture.width, picture.height);
        let chroma_size = (size.0 / 2, size.1 / 2);
        let see_through = picture.alpha.is_some();
        let fits = |held: &Slot| held.size == size && held.see_through == see_through;
        if self.slots.get(slot).is_none_or(|held| !fits(held)) {
            let luma = plane_texture(device, wgpu::TextureFormat::R8Unorm, size);
            let chroma = plane_texture(device, wgpu::TextureFormat::Rg8Unorm, chroma_size);
            let alpha_size = if see_through { size } else { (1, 1) };
            let alpha = plane_texture(device, wgpu::TextureFormat::R8Unorm, alpha_size);
            if !see_through {
                upload(queue, &alpha, &[u8::MAX], 1, (1, 1));
            }
            let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cannotcut viewer"),
                size: UNIFORM_BYTES as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let view = |texture: &wgpu::Texture| {
                texture.create_view(&wgpu::TextureViewDescriptor::default())
            };
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cannotcut viewer"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view(&luma)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&view(&chroma)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&view(&alpha)),
                    },
                ],
            });
            let made = Slot {
                size,
                luma,
                chroma,
                alpha,
                see_through,
                uniforms,
                bind_group,
                holds: None,
            };
            if slot < self.slots.len() {
                self.slots[slot] = made;
            } else {
                self.slots.push(made);
            }
        }
        let held = &mut self.slots[slot];
        if held.holds != Some(picture.serial) {
            upload(queue, &held.luma, &picture.luma, size.0, size);
            // Two bytes per chroma sample, so a row is as long as a luma row.
            upload(queue, &held.chroma, &picture.chroma, size.0, chroma_size);
            if let Some(alpha) = &picture.alpha {
                upload(queue, &held.alpha, alpha, size.0, size);
            }
            held.holds = Some(picture.serial);
        }

        let mut uniforms = [0.0; UNIFORM_FLOATS];
        uniforms[..16].copy_from_slice(&to_rgb(picture.colors));
        uniforms[16..22].copy_from_slice(&placed.corners);
        uniforms[22..24].copy_from_slice(&placed.frame_share);
        uniforms[24] = placed.opacity;
        uniforms[25] = f32::from(u8::from(self.linear_target));
        uniforms[28..31].copy_from_slice(&placed.wipe);
        let mut bytes = [0; UNIFORM_BYTES];
        for (chunk, value) in bytes.chunks_exact_mut(size_of::<f32>()).zip(uniforms) {
            chunk.copy_from_slice(&value.to_ne_bytes());
        }
        queue.write_buffer(&held.uniforms, 0, &bytes);
    }
}

fn plane_texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    (width, height): (u32, u32),
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cannotcut viewer plane"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn upload(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    rows: &[u8],
    bytes_per_row: u32,
    (width, height): (u32, u32),
) {
    queue.write_texture(
        texture.as_image_copy(),
        rows,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// The share of `bounds` a picture covers when it is scaled to fit inside without distortion.
fn covered(picture: (u32, u32), bounds: Size) -> [f32; 2] {
    let picture_aspect = picture.0 as f32 / picture.1.max(1) as f32;
    let bounds_aspect = bounds.width / bounds.height.max(1.0);
    if picture_aspect > bounds_aspect {
        [1.0, bounds_aspect / picture_aspect]
    } else {
        [picture_aspect / bounds_aspect, 1.0]
    }
}

/// The matrix taking (Y, Cb, Cr, 1), as sampled from 8-bit textures, to gamma-encoded RGB.
/// Column-major, as the shader reads it.
fn to_rgb(colors: Colors) -> [f32; 16] {
    let (red_weight, blue_weight) = match colors.matrix {
        Matrix::Bt601 => (0.299, 0.114),
        Matrix::Bt709 => (0.2126, 0.0722),
        Matrix::Bt2020 => (0.2627, 0.0593),
    };
    let green_weight = 1.0 - red_weight - blue_weight;
    // Limited range puts black at code 16 and white at 235, and spreads chroma over 16 to 240.
    let (luma_gain, chroma_gain, black) = if colors.full_range {
        (1.0, 1.0, 0.0)
    } else {
        (255.0 / 219.0, 255.0 / 224.0, 16.0 / 255.0)
    };
    let red_cr = 2.0 * (1.0 - red_weight) * chroma_gain;
    let blue_cb = 2.0 * (1.0 - blue_weight) * chroma_gain;
    let green_cb = -2.0 * blue_weight * (1.0 - blue_weight) / green_weight * chroma_gain;
    let green_cr = -2.0 * red_weight * (1.0 - red_weight) / green_weight * chroma_gain;
    let offset = -luma_gain * black;
    #[rustfmt::skip]
    let matrix = [
        luma_gain, luma_gain, luma_gain, 0.0,
        0.0, green_cb, blue_cb, 0.0,
        red_cr, green_cr, 0.0, 0.0,
        offset - red_cr * NEUTRAL_CHROMA,
        offset - (green_cb + green_cr) * NEUTRAL_CHROMA,
        offset - blue_cb * NEUTRAL_CHROMA,
        1.0,
    ];
    matrix
}

/// Finds out which of iced's renderers draws the window: only wgpu runs custom shaders, and iced
/// falls back to its software renderer when it finds no usable GPU. Takes no space; keep it in
/// the view until it has answered.
pub struct RendererProbe<Message> {
    on_answer: fn(bool) -> Message,
}

/// `on_answer` gets true when the window is drawn on the GPU.
pub fn renderer_probe<Message>(on_answer: fn(bool) -> Message) -> RendererProbe<Message> {
    RendererProbe { on_answer }
}

impl<Message, Theme> Widget<Message, Theme, iced::Renderer> for RendererProbe<Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(Size::ZERO)
    }

    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        _event: &Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        // The renderer is only within reach while handling an event; the first one will do.
        shell.publish((self.on_answer)(matches!(
            renderer,
            iced::Renderer::Primary(_)
        )));
    }
}

impl<'a, Message: 'a, Theme> From<RendererProbe<Message>>
    for Element<'a, Message, Theme, iced::Renderer>
{
    fn from(probe: RendererProbe<Message>) -> Self {
        Element::new(probe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(colors: Colors, code: [u8; 3]) -> [f32; 3] {
        let matrix = to_rgb(colors);
        let sampled = [
            f32::from(code[0]) / 255.0,
            f32::from(code[1]) / 255.0,
            f32::from(code[2]) / 255.0,
            1.0,
        ];
        std::array::from_fn(|row| {
            (0..4)
                .map(|column| matrix[column * 4 + row] * sampled[column])
                .sum()
        })
    }

    fn assert_close(actual: [f32; 3], expected: [f32; 3]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 0.01, "{actual} is not {expected}");
        }
    }

    #[test]
    fn limited_range_reaches_black_and_white() {
        for matrix in [Matrix::Bt601, Matrix::Bt709, Matrix::Bt2020] {
            let colors = Colors {
                matrix,
                full_range: false,
            };
            assert_close(convert(colors, [16, 128, 128]), [0.0; 3]);
            assert_close(convert(colors, [235, 128, 128]), [1.0; 3]);
        }
    }

    #[test]
    fn full_range_reaches_black_and_white() {
        let colors = Colors {
            matrix: Matrix::Bt709,
            full_range: true,
        };
        assert_close(convert(colors, [0, 128, 128]), [0.0; 3]);
        assert_close(convert(colors, [255, 128, 128]), [1.0; 3]);
    }

    #[test]
    fn primaries_come_out_pure() {
        let bt709 = Colors {
            matrix: Matrix::Bt709,
            full_range: false,
        };
        assert_close(convert(bt709, [63, 102, 240]), [1.0, 0.0, 0.0]);
        assert_close(convert(bt709, [32, 240, 118]), [0.0, 0.0, 1.0]);
        let bt601 = Colors {
            matrix: Matrix::Bt601,
            full_range: false,
        };
        assert_close(convert(bt601, [81, 90, 240]), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn pictures_are_fitted_inside_the_viewer() {
        assert_eq!(covered((1920, 1080), Size::new(1600.0, 900.0)), [1.0, 1.0]);
        assert_eq!(covered((1920, 1080), Size::new(1600.0, 1800.0)), [1.0, 0.5]);
        assert_eq!(covered((1080, 1920), Size::new(1920.0, 1920.0)), [0.5625, 1.0]);
    }
}
