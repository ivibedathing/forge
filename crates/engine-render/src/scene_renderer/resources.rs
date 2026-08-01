use super::*;

/// One uploaded mesh, cached across frames.
///
/// Positions and normals share `vertices`; `normals_offset` is where the
/// second slot starts. `_geometry` keeps the source `Arc` alive: the cache is
/// keyed on that allocation's address, and holding a strong reference is what
/// stops a freed mesh's address from being reused by a *different* mesh and
/// silently colliding.
pub(crate) struct CachedMesh {
    pub(crate) _geometry: Arc<MeshData>,
    pub(crate) vertices: wgpu::Buffer,
    pub(crate) normals_offset: u64,
    /// Where the UVs start in the same buffer. Uploaded for every mesh since
    /// M23, because a road's markings are painted from surface coordinates it
    /// carries there — before that nothing on the GPU read a UV at all.
    pub(crate) uvs_offset: u64,
    /// Where the skinning influences start in the same buffer (M30), for a
    /// skinned mesh. `None` for everything else — which is every mesh
    /// committed before M30 — so no existing vertex buffer grew by a byte.
    pub(crate) skin_offsets: Option<SkinOffsets>,
    pub(crate) indices: wgpu::Buffer,
    pub(crate) index_count: u32,
    /// Frame counter at the last draw that used this mesh; entries idle for
    /// [`MESH_CACHE_LIFETIME`] frames are dropped.
    pub(crate) last_used: u64,
}

/// Where a skinned mesh's two extra vertex slots start in its shared buffer.
#[derive(Clone, Copy)]
pub(crate) struct SkinOffsets {
    pub(crate) joints: u64,
    pub(crate) weights: u64,
}

/// One uploaded meadow, cached across frames (M29).
///
/// Three buffers rather than two, because a meadow is the one thing in this
/// engine drawn instanced from geometry of its own: the plant template, its
/// indices, and where every copy of it stands. All three are static — the life
/// cycle is evaluated in the vertex stage — so this uploads once and is never
/// rewritten however many generations pass.
///
/// `_patch` keeps the source `Arc` alive for `CachedMesh`'s reason: the cache is
/// keyed on that allocation's address, and holding a strong reference is what
/// stops a freed patch's address from being reused by a different one.
pub(crate) struct CachedMeadow {
    pub(crate) _patch: Arc<engine_core::meadow::MeadowPatch>,
    pub(crate) vertices: wgpu::Buffer,
    pub(crate) indices: wgpu::Buffer,
    pub(crate) instances: wgpu::Buffer,
    pub(crate) index_count: u32,
    pub(crate) instance_count: u32,
    pub(crate) last_used: u64,
}

/// How many frames an unused mesh stays uploaded. Long enough that a scene
/// alternating between two sets of geometry does not re-upload every frame,
/// short enough that editing a scene down to a few entities gives the memory
/// back promptly.
pub(crate) const MESH_CACHE_LIFETIME: u64 = 240;

/// A uniform buffer plus the bind group naming it, recreated only when the
/// buffer has to grow.
pub(crate) struct Uniforms {
    pub(crate) buffer: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
    /// Capacity in bytes.
    pub(crate) size: u64,
}

/// The HUD overlay's cached texture. Kept at the largest size any frame has
/// needed so far — canvases are small (they cover only what the HUD touches)
/// and a growing one is rare after the first few frames.
pub(crate) struct HudTarget {
    pub(crate) texture: wgpu::Texture,
    pub(crate) bind_group: wgpu::BindGroup,
    pub(crate) placement: wgpu::Buffer,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// The opaque pass's depth, copied where the water pass can read it (M18).
///
/// A pass cannot sample the depth attachment it is testing against, so the
/// frame gains a fullscreen copy between the opaque geometry and the water:
/// `Depth32Float` (possibly multisampled) → single-sampled `R32Float`. Water is
/// the only thing that reads it, so it is allocated the first time a scene has
/// any, and resized when the target does.
///
/// Since M26 the view rides in the shared frame-textures group rather than in a
/// group of its own — see [`FrameTextures`].
pub(crate) struct SceneDepth {
    pub(crate) view: wgpu::TextureView,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl SceneDepth {
    pub(crate) fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene-depth-copy"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // R32Float, not a depth format: this is read with `textureLoad` as
            // an ordinary float, and depth formats cannot be sampled that way.
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            view,
            width: width.max(1),
            height: height.max(1),
        }
    }

    /// The copy for a target of this size, reallocated when the size changes.
    /// Exact rather than grow-only: the shader converts pixel coordinates to
    /// UVs with this texture's own dimensions, so a stale larger copy would
    /// read the wrong pixels rather than merely waste memory.
    ///
    /// Returns whether it reallocated, which is what tells the frame-textures
    /// bind group that it has to be rebuilt.
    pub(crate) fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> bool {
        let fits = slot
            .as_ref()
            .is_some_and(|held| held.width == width.max(1) && held.height == height.max(1));
        if !fits {
            *slot = Some(Self::new(device, width, height));
        }
        !fits
    }
}

/// The opaque pass's colour, copied where a refracting surface can read it
/// (M26). Allocated the first time a scene refracts anything and resized with
/// the target, exactly like [`SceneDepth`], and for the same reason: a pass
/// cannot sample the colour attachment it is drawing into.
pub(crate) struct SceneColor {
    pub(crate) view: wgpu::TextureView,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl SceneColor {
    pub(crate) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene-color-copy"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            view,
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub(crate) fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> bool {
        let fits = slot
            .as_ref()
            .is_some_and(|held| held.width == width.max(1) && held.height == height.max(1));
        if !fits {
            *slot = Some(Self::new(device, format, width, height));
        }
        !fits
    }
}

/// Group 2 for every lit pipeline: the shadow map, the depth copy, and the
/// colour copy, with their samplers (M26).
///
/// They were three groups only because they arrived in three milestones, and
/// three of four slots is what the budget could not afford — see
/// `designs/material-system-design.md` §3. Every one of them is frame-scoped:
/// written once per frame, read by everything, rebuilt only when the render
/// target resizes or a scene starts casting shadows.
///
/// The bind group is cached against `key` so the steady-state frame rebuilds
/// nothing; a 1×1 placeholder stands in for each texture the frame does not
/// have, since WGSL binds unconditionally while the reads sit behind a branch.
pub(crate) struct FrameTextures {
    pub(crate) bind_group: wgpu::BindGroup,
    /// The same group with the **colour copy left out** — a 1×1 placeholder in
    /// its slot — for the opaque pass.
    ///
    /// Not an optimisation: on the refracting path the opaque pass is *drawing
    /// into* that copy (directly without MSAA, as a resolve target with it), and
    /// a texture cannot be a colour attachment and a bound resource in the same
    /// pass. Nothing in the opaque pass reads scene colour, so leaving it out
    /// costs nothing and the two groups share every other binding.
    pub(crate) opaque_bind_group: wgpu::BindGroup,
    pub(crate) key: FrameTextureKey,
}

/// What a cached [`FrameTextures`] was built from. The sizes are the copies'
/// own dimensions, so a resize invalidates the group by construction.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameTextureKey {
    pub(crate) shadows: bool,
    pub(crate) depth: Option<(u32, u32)>,
    pub(crate) color: Option<(u32, u32)>,
}

/// What a cascaded frame needs beyond what M16 allocated (M38).
///
/// Built once, at construction, and only beyond one cascade.
///
/// The caster half is the part worth explaining. `shadow.wgsl` and its three
/// siblings read `frame.light_view_proj`, and cascade *i* needs a different
/// one. Rather than teach four caster shaders about cascades — or make the
/// frame bind group layout dynamic, which every pipeline in the engine shares —
/// the frame uniform is written **once per cascade** into one buffer at aligned
/// offsets, and each cascade's pass binds a group naming its own slice. The
/// caster shaders and the layout are untouched; `record_shadows` gains a loop.
pub(crate) struct CascadeResources {
    /// The matrices the receivers sample through, at group 2 binding 5.
    pub(crate) matrices: wgpu::Buffer,
    /// The frame uniform, once per cascade.
    pub(crate) caster_frames: wgpu::Buffer,
    /// A group-1 binding per cascade, over that cascade's slice of it.
    pub(crate) caster_groups: Vec<wgpu::BindGroup>,
    /// The frame uniform's size rounded up to the device's uniform alignment.
    pub(crate) caster_stride: u64,
}

impl CascadeResources {
    pub(crate) fn new(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
        cascades: u32,
    ) -> Self {
        let matrices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cascade-matrices"),
            size: std::mem::size_of::<CascadeUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
        let frame_size = std::mem::size_of::<FrameUniform>() as u64;
        let caster_stride = frame_size.next_multiple_of(alignment);
        let caster_frames = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cascade-caster-frames"),
            size: caster_stride * u64::from(cascades),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let caster_groups = (0..cascades)
            .map(|cascade| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("cascade-caster-frame"),
                    layout: frame_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &caster_frames,
                            offset: caster_stride * u64::from(cascade),
                            size: std::num::NonZeroU64::new(frame_size),
                        }),
                    }],
                })
            })
            .collect();

        Self {
            matrices,
            caster_frames,
            caster_groups,
            caster_stride,
        }
    }
}

/// One uploaded texture with its mip chain, cached across frames.
///
/// `_source` keeps the `TextureData` alive for exactly the reason `CachedMesh`
/// keeps its geometry: the cache is keyed on that allocation's address, and a
/// strong reference is what stops a freed texture's address from being reused
/// by a *different* one and silently colliding.
pub(crate) struct CachedTexture {
    pub(crate) _source: Arc<engine_core::texture::TextureData>,
    pub(crate) view: wgpu::TextureView,
    pub(crate) last_used: u64,
}

/// A material's four map slots as one bind group, cached on the identities of
/// the textures in it.
///
/// Keyed on the `Arc` addresses rather than on the entity, because two entities
/// sharing a `materials/*.json` share its pixels and should share its bind
/// group — which is most of the point of shareable materials.
pub(crate) struct CachedMaterial {
    pub(crate) bind_group: wgpu::BindGroup,
    pub(crate) last_used: u64,
}

/// The 1×1 white bound in a material slot with no map.
///
/// **Written, not merely allocated.** The reads sit behind the `map_params`
/// bits so nothing should ever sample it — but "should" is doing work there,
/// and an unwritten texture's contents are whatever the allocator last had.
/// Leaving it undefined cost an afternoon: a slot that was in fact being bound
/// rendered as a stable magenta that looked exactly like a mip-chain bug.
pub(crate) fn white_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("material-white"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[255u8, 255, 255, 255],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// A 1×1 texture of `format`, bound wherever a frame does not have the real
/// thing. Never read: every sampler of it sits behind a branch that is off.
pub(crate) fn placeholder_texture(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

impl Uniforms {
    /// `binding_size` is the size of one binding when the buffer holds an
    /// array addressed by dynamic offset; `None` binds the whole buffer.
    pub(crate) fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        label: &str,
        size: u64,
        binding_size: Option<u64>,
    ) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(wgpu::COPY_BUFFER_ALIGNMENT),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: binding_size.and_then(std::num::NonZeroU64::new),
                }),
            }],
        });
        Self {
            buffer,
            bind_group,
            size,
        }
    }

    /// The uniforms at `slot`, allocated or grown to hold `size` bytes. Only
    /// a growth reallocates — a steady-state frame reuses everything.
    pub(crate) fn ensure<'a>(
        slot: &'a mut Option<Self>,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        label: &str,
        size: u64,
        binding_size: Option<u64>,
    ) -> &'a Self {
        if slot.as_ref().is_none_or(|held| held.size < size) {
            *slot = Some(Self::new(device, layout, label, size, binding_size));
        }
        slot.as_ref().expect("just ensured")
    }
}

impl HudTarget {
    /// The overlay texture, allocated or grown to hold a `width × height`
    /// canvas. Canvases shrink and grow with the HUD's content, so the
    /// texture keeps the largest size seen and writes smaller canvases into
    /// its corner; the shader only ever reads the written region.
    pub(crate) fn ensure<'a>(
        targets: &'a mut Vec<Self>,
        index: usize,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
    ) -> &'a Self {
        let held = targets.get(index);
        let fits = held.is_some_and(|held| held.width >= width && held.height >= height);
        if !fits {
            let (width, height) = match held {
                Some(held) => (held.width.max(width), held.height.max(height)),
                None => (width, height),
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("hud-overlay"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // sRGB like the render target: `textureLoad` decodes, the
                // target re-encodes, and the round trip is byte-exact.
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let placement = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hud-placement"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("hud-bind-group"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: placement.as_entire_binding(),
                    },
                ],
            });
            let target = Self {
                texture,
                bind_group,
                placement,
                width,
                height,
            };
            match targets.get_mut(index) {
                Some(slot) => *slot = target,
                // Canvas counts only grow by one at a time as the HUD gains
                // separated elements, so the index is always the next slot.
                None => targets.insert(index.min(targets.len()), target),
            }
        }
        &targets[index]
    }
}

/// A buffer holding `contents`, created once and never rewritten — the shape
/// mesh geometry wants.
pub(crate) fn buffer_with(
    device: &wgpu::Device,
    label: &str,
    usage: wgpu::BufferUsages,
    contents: &[u8],
) -> wgpu::Buffer {
    use wgpu::util::DeviceExt as _;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage,
    })
}

/// The buffer at `slot`, allocated or grown to hold `size` bytes. Growth
/// doubles, so a particle system that ramps up settles after a few frames
/// instead of reallocating on every new particle.
pub(crate) fn grow_buffer<'a>(
    slot: &'a mut Option<wgpu::Buffer>,
    device: &wgpu::Device,
    label: &str,
    usage: wgpu::BufferUsages,
    size: u64,
) -> &'a wgpu::Buffer {
    if slot.as_ref().is_none_or(|held| held.size() < size) {
        let capacity = slot
            .as_ref()
            .map_or(size, |held| (held.size() * 2).max(size))
            .max(wgpu::COPY_BUFFER_ALIGNMENT);
        *slot = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: capacity,
            usage,
            mapped_at_creation: false,
        }));
    }
    slot.as_ref().expect("just ensured")
}
