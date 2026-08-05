use super::*;

/// The facts one recipe pass's pipeline actually differs in. Everything else
/// about a recipe's `RenderPipelineDescriptor` — triangle list, `vs_main` /
/// `fs_main`, full colour writes, `DEPTH_FORMAT`, no multiview, no cache —
/// was repeated verbatim by five constructors (~35 lines each) before
/// [`recipe_pipeline`] absorbed it. `build_skinned`'s `mesh_pipeline` closure
/// proved the shape; this is the same move for the unskinned passes.
///
/// A descriptor is plain data — identical fields build an identical pipeline —
/// so this is pure code motion with no bearing on any ULP-sensitive path,
/// which live in shader *text*, not in pipeline state.
struct RecipePipeline<'a> {
    label: &'static str,
    shader: &'a wgpu::ShaderModule,
    layout: &'a wgpu::PipelineLayout,
    buffers: &'a [Option<wgpu::VertexBufferLayout<'a>>],
    blend: wgpu::BlendState,
    cull_mode: Option<wgpu::Face>,
    depth_write: bool,
    depth_compare: wgpu::CompareFunction,
}

/// Build one recipe pass from its [`RecipePipeline`] facts.
fn recipe_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    multisample: wgpu::MultisampleState,
    recipe: RecipePipeline<'_>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(recipe.label),
        layout: Some(recipe.layout),
        vertex: wgpu::VertexState {
            module: recipe.shader,
            entry_point: Some("vs_main"),
            buffers: recipe.buffers,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: recipe.shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(recipe.blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: recipe.cull_mode,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(recipe.depth_write),
            depth_compare: Some(recipe.depth_compare),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample,
        multiview_mask: None,
        cache: None,
    })
}

/// Everything a skinned draw needs, built the **first time a frame has one**
/// (M30) and kept for the life of the renderer.
///
/// Lazy for the reason the shadow map, the 1×1 white texture and the colour
/// copy are: a scene with no skinned mesh pays nothing, and "nothing" here is
/// six shader compilations at startup — which every `engine screenshot` in this
/// repo but one would otherwise pay on every invocation.
///
/// The variants mirror the unskinned ones exactly, so routing a skinned draw is
/// the same decision with the same inputs; anything else would be a second
/// place for "which pipeline does this material want" to disagree with itself.
pub(crate) struct SkinnedPipelines {
    pub(crate) opaque: wgpu::RenderPipeline,
    pub(crate) textured: wgpu::RenderPipeline,
    pub(crate) transparent: wgpu::RenderPipeline,
    pub(crate) textured_transparent: wgpu::RenderPipeline,
    pub(crate) refractive: wgpu::RenderPipeline,
    pub(crate) shadow: wgpu::RenderPipeline,
    pub(crate) shadow_cutout: wgpu::RenderPipeline,
}

/// Everything a foliage draw needs (M46), built the **first time a frame has
/// one** and kept for the life of the renderer.
///
/// Lazy on `SkinnedPipelines`' precedent and for its reason: four shader
/// compilations that a scene with no tree in it should not pay for.
///
/// Four variants rather than the skinned set's seven. Leaves are opaque by
/// construction (`Tree::leaf_material`), so the only way to reach the blended
/// pass with foliage is a *transparent bark material* — which
/// `tree_sway_needs_opaque_bark` warns about instead, because the alternative
/// is three more pipelines for a case no scene has and the design says so
/// (M46 §7). Everything else about them mirrors the unskinned pipelines
/// exactly, down to the front-face-culled caster.
pub(crate) struct FoliagePipelines {
    pub(crate) opaque: wgpu::RenderPipeline,
    pub(crate) textured: wgpu::RenderPipeline,
    pub(crate) shadow: wgpu::RenderPipeline,
    pub(crate) shadow_cutout: wgpu::RenderPipeline,
}

/// Which optional vertex slots and bind groups one skinned pipeline declares.
///
/// Two variants' worth of difference, spelled out rather than inferred: a
/// vertex-buffer slot bound in the wrong order is a character that renders as
/// noise, and there is no way to see from the noise which slot was wrong.
#[derive(Clone, Copy)]
pub(crate) struct SkinnedInputs {
    /// Whether the stage reads a normal. The solid caster does not.
    pub(crate) normal: bool,
    /// The material's maps and the group they bind at — 3 in the mesh passes,
    /// 2 in the cut-out caster, which has no frame textures to read because it
    /// *is* what writes one of them.
    pub(crate) material: Option<([usize; 4], u32)>,
}

impl SkinnedInputs {
    pub(crate) const CASTER: Self = Self {
        normal: false,
        material: None,
    };
    pub(crate) const LIT: Self = Self {
        normal: true,
        material: None,
    };
    pub(crate) fn textured(key: [usize; 4]) -> Self {
        Self {
            normal: true,
            material: Some((key, 3)),
        }
    }
    pub(crate) fn cutout_caster(key: [usize; 4]) -> Self {
        Self {
            normal: true,
            material: Some((key, 2)),
        }
    }
}

/// The palette buffer and the group-0 bind group naming it beside the object
/// uniforms.
///
/// Rebuilt when either buffer is reallocated, which the recorded capacities
/// detect: a bind group holds its buffers by identity, and `Uniforms::ensure`
/// mints a new one whenever the draw list outgrows the old.
pub(crate) struct SkinnedObjects {
    pub(crate) palette: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
    pub(crate) palette_size: u64,
    pub(crate) objects_size: u64,
}

impl super::SceneRenderer {
    /// A renderer whose scene pipelines are built for `samples`-way MSAA.
    ///
    /// The sample count is baked into every pipeline, so it belongs to the
    /// renderer rather than to a frame: a scene that changes `samples` gets a
    /// new `SceneRenderer`, which is what the viewer's reload path does.
    pub fn with_samples(device: &wgpu::Device, format: wgpu::TextureFormat, samples: u32) -> Self {
        Self::configured(device, format, samples, 1, false)
    }

    /// A renderer built for `samples`-way MSAA, `cascades` shadow maps, and
    /// `gi` saying whether the scene has a `LightProbeVolume` (M35).
    ///
    /// All three are baked into every pipeline, and for the same reason: the
    /// sample count is pipeline state, the cascade count decides whether the
    /// shadow map is bound as a `texture_depth_2d` or a `texture_depth_2d_array`
    /// — which is a bind group *layout*, not a uniform — and GI is spliced into
    /// every mesh variant's shader source. A scene that changes any of them gets
    /// a new `SceneRenderer`, which is what the viewer's reload path does.
    ///
    /// At one cascade with `gi` false every pipeline here compiles the shader
    /// source that sits on disk, unmodified. That is the property every
    /// committed baseline rests on — see M38 §4 and M35 §7.
    pub fn configured(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        samples: u32,
        cascades: u32,
        gi: bool,
    ) -> Self {
        let samples = samples.max(1);
        let cascades = cascades.clamp(1, MAX_SHADOW_CASCADES);
        let multisample = wgpu::MultisampleState {
            count: samples,
            ..Default::default()
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_cascades(
                &plain_mesh(gi),
                cascades,
            ))),
        });

        let uniform_layout = |label: &str, binding_size: Option<u64>| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        // A dynamic offset only makes sense against a binding
                        // smaller than the buffer: the binding is one struct,
                        // the buffer is the whole array of them.
                        has_dynamic_offset: binding_size.is_some(),
                        min_binding_size: binding_size.and_then(std::num::NonZeroU64::new),
                    },
                    count: None,
                }],
            })
        };
        // One buffer holds every entity's uniforms; each draw selects its own
        // with a dynamic offset, so the whole draw list is one bind group and
        // one upload rather than one of each per entity.
        let object_layout = uniform_layout(
            "object-uniforms",
            Some(std::mem::size_of::<ObjectUniform>() as u64),
        );
        let frame_layout = uniform_layout("frame-uniforms", None);

        // The same group 0 with the joint palette beside it (M27). A second
        // *layout*, not a second group index: `downlevel_defaults` caps
        // `max_bind_groups` at 4 and M26 spent the fourth, so the palette rides
        // in group 0 under a layout the skinned pipelines alone use — which
        // costs the plain pipelines nothing, because they keep theirs.
        let skinned_object_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("skinned-object-uniforms"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<
                                ObjectUniform,
                            >(
                            )
                                as u64),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        // Vertex only: the palette moves vertices and nothing
                        // in a fragment stage has ever asked where a joint is.
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<
                                JointPaletteUniform,
                            >(
                            )
                                as u64),
                        },
                        count: None,
                    },
                ],
            });

        // Group 2: the frame's textures — the shadow map and its comparison
        // sampler, the opaque depth copy, and the opaque colour copy with the
        // sampler that reads it (M26). Every entry is always present in the
        // layout even when the frame has none of them: a bind group layout may
        // contain entries the shader never references, and the reverse is the
        // error, so `mesh.wgsl` keeps declaring only bindings 0 and 1 and stays
        // the file it has been since M4.
        //
        // Merging these was a bind-group-budget decision, not a tidiness one:
        // `downlevel_defaults` caps `max_bind_groups` at 4, and three of them
        // spent on frame-scoped textures left nowhere for a material.
        let mut frame_texture_entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: frame_binding::SHADOW_MAP,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    // One cascade is a plain 2D map, exactly as it has
                    // been since M16; beyond one it is an array, and the
                    // four receivers are spliced to match (M38).
                    view_dimension: if cascades == 1 {
                        wgpu::TextureViewDimension::D2
                    } else {
                        wgpu::TextureViewDimension::D2Array
                    },
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: frame_binding::SHADOW_SAMPLER,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: frame_binding::SCENE_DEPTH,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    // Read with `textureLoad`: no sampler, so nothing
                    // filters depth across a silhouette.
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: frame_binding::SCENE_COLOR,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: frame_binding::SCENE_SAMPLER,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        // The cascade matrices, beside the map they address (M38). Present only
        // in the cascaded layout: at one cascade the group is the one M26 left,
        // entry for entry, and the four receivers declare what they always did.
        if cascades > 1 {
            frame_texture_entries.push(wgpu::BindGroupLayoutEntry {
                binding: frame_binding::CASCADES,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(
                        std::mem::size_of::<CascadeUniform>() as u64,
                    ),
                },
                count: None,
            });
        }
        // The four SH-L1 coefficient planes of the irradiance field (M35), and
        // the sampler that reads them. Unconditional, unlike the cascade buffer
        // above — a 1x1x1 placeholder is bound where a scene has no probe
        // volume, because a layout may carry entries the shader never
        // references and the reverse is the error. Bindings 6-10, stepping over
        // the 5 M38 holds.
        //
        // `Rgba16Float` is filterable in core WebGPU, which is the entire reason
        // the field is a 3D texture rather than a buffer — probe interpolation
        // comes free and continuous from the sampler.
        frame_texture_entries.extend(
            (frame_binding::GI_SH_FIRST..frame_binding::GI_SH_FIRST + 4).map(|binding| {
                wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                }
            }),
        );
        frame_texture_entries.push(wgpu::BindGroupLayoutEntry {
            binding: frame_binding::GI_SAMPLER,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
        let frame_textures_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("frame-textures"),
                entries: &frame_texture_entries,
            });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh-pipeline-layout"),
            bind_group_layouts: &[
                Some(&object_layout),
                Some(&frame_layout),
                Some(&frame_textures_layout),
            ],
            immediate_size: 0,
        });

        // Position, normal and UV live in separate buffers so a mesh with no
        // normals does not need a padded interleaved layout. Only the road
        // pipeline binds the third — it is where a road's surface coordinates
        // travel.
        let vertex_layouts = [
            Some(wgpu::VertexBufferLayout {
                array_stride: 12,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                }],
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: 12,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x3,
                }],
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x2,
                }],
            }),
        ];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts[..2],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        });

        // Group 3 for a textured mesh (M26): the four maps and one sampler.
        // Every slot is always present in the layout, with a 1×1 white bound
        // where a material has no map, because WGSL binds unconditionally and
        // the reads sit behind the `map_params` bits.
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material-maps"),
            entries: &[0u32, 1, 2, 3]
                .map(|binding| wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                })
                .into_iter()
                .chain([wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                }])
                .collect::<Vec<_>>(),
        });
        let map_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material-sampler"),
            // Repeat on both axes, because tiling is what a material texture is
            // for: `ClampToEdge` would make `uv_scale: [20, 20]` draw one
            // stretched copy surrounded by smeared border pixels.
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            // Anisotropy is pinned at 1 (off) in v1. It measurably improves
            // exactly the grazing-angle tiling this milestone is for, and it is
            // also a per-adapter *quality* setting — which is where this repo
            // has repeatedly found reproducibility to die. A baseline should be
            // a function of the scene, not of the driver's filtering.
            anisotropy_clamp: 1,
            ..Default::default()
        });

        let textured_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("textured-pipeline-layout"),
            bind_group_layouts: &[
                Some(&object_layout),
                Some(&frame_layout),
                Some(&frame_textures_layout),
                Some(&material_layout),
            ],
            immediate_size: 0,
        });

        // The terrain twin of the mesh pipeline (M22): identical in every
        // respect except its shader module, which is `mesh.wgsl` with the
        // generative material spliced in by `with_terrain`.
        //
        // A second pipeline rather than a branch inside one, because the branch
        // was measured and it cost `m16_environment`, `m17_fire` and
        // `m18_water` one pixel each. Compiling the untouched file for
        // everything that is not terrain is the only way to be byte-identical
        // by construction rather than by hoping.
        let terrain_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_cascades(
                &with_terrain(gi),
                cascades,
            ))),
        });
        let terrain_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &terrain_shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts[..2],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &terrain_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        });

        // The textured twins (M26). Same states as the plain and blended mesh
        // pipelines exactly — a textured surface is not a differently-composited
        // surface, it is a differently-*resolved* one — with a fourth bind group
        // for the maps and a third vertex slot for the UVs they are read at.
        let textured_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("textured-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_cascades(
                &with_textures(gi),
                cascades,
            ))),
        });
        let textured_blended_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("textured-blended-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_cascades(
                &with_textures_and_refraction(gi),
                cascades,
            ))),
        });
        let textured_pipeline_for = |label: &str,
                                     module: &wgpu::ShaderModule,
                                     blend: wgpu::BlendState,
                                     depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&textured_layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers: &vertex_layouts,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };
        let textured_pipeline = textured_pipeline_for(
            "textured-pipeline",
            &textured_shader,
            wgpu::BlendState::REPLACE,
            true,
        );
        let textured_transparent_pipeline = textured_pipeline_for(
            "textured-transparent-pipeline",
            &textured_blended_shader,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            false,
        );

        // The blended twin of the mesh pipeline: same shader, same layout,
        // same geometry. What differs is that it must not write depth (two
        // transparent surfaces have to blend with each other rather than the
        // nearer one masking the farther) and that its blend factors expect
        // the premultiplied color `fs_main` produces for these materials.
        let transparent_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-transparent-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts[..2],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        });

        // Refraction is a *third* blended pipeline rather than a branch inside
        // the second, and that was measured, not assumed: compiling the
        // refraction variant for every transparent draw moved one pixel of
        // `m16_environment.png` by one channel step — M22's lesson repeating on
        // the one fixture that has transmissive geometry. The added branch is
        // never taken by a surface with `ior: 1.0` and `thickness: 0.0`, and it
        // still changes the code the compiler sees around M16's untouchable
        // lines, which is exactly what the rule is about.
        //
        // So a material that does not refract keeps the pipeline it had, whose
        // module is `mesh.wgsl` as it sits on disk, and only a material that
        // asks to bend light pays for a second shader.
        let refractive_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh-refractive-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_cascades(
                &with_refraction(gi),
                cascades,
            ))),
        });
        let refractive_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-refractive-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &refractive_shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts[..2],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &refractive_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        });

        let shadow_pipeline =
            Self::shadow_pipeline(device, &object_layout, &frame_layout, &vertex_layouts[..1]);
        let shadow_cutout_pipeline = Self::shadow_cutout_pipeline(
            device,
            &object_layout,
            &frame_layout,
            &material_layout,
            &vertex_layouts,
        );
        let sky_pipeline = Self::sky_pipeline(device, &frame_layout, format, multisample);

        // Water (M18). Its own uniform, its own shader, and the mesh pass's
        // frame and frame-texture bindings — the depth it absorbs against
        // arrives in group 2 with the shadow map since M26, which is what frees
        // its group 3.
        let water_layout = uniform_layout(
            "water-uniforms",
            Some(std::mem::size_of::<WaterUniform>() as u64),
        );
        let water_pipeline = Self::water_pipeline(
            device,
            &water_layout,
            &frame_layout,
            &frame_textures_layout,
            // Position only: the grid's stored normals are the flat ones, and
            // the real normal comes from the wave derivatives.
            &vertex_layouts[..1],
            format,
            multisample,
            with_cascades(include_str!("../shaders/water.wgsl"), cascades),
        );
        // The same again with refraction spliced in (M27), for the surfaces
        // that bend what is behind them. A second pipeline rather than a
        // branch, for M22's and M26's measured reason: compiling the variant
        // for every water draw is a change to the code around the M18 shader's
        // arithmetic, and that is enough to move a pixel in a pond that does
        // not refract.
        let refractive_water_pipeline = Self::water_pipeline(
            device,
            &water_layout,
            &frame_layout,
            &frame_textures_layout,
            &vertex_layouts[..1],
            format,
            multisample,
            with_cascades(&with_water_refraction(), cascades)
                .into_owned()
                .into(),
        );
        // Clouds (M20). Its own uniform and shader, the mesh pass's frame
        // binding, and nothing else: no shadow map (the engine has one cascade
        // and it belongs to the ground) and no scene depth (a cloud is not
        // absorbing what is behind it).
        let cloud_layout = uniform_layout(
            "cloud-uniforms",
            Some(std::mem::size_of::<CloudUniform>() as u64),
        );
        let cloud_pipeline = Self::cloud_pipeline(
            device,
            &cloud_layout,
            &frame_layout,
            &vertex_layouts[..2],
            format,
            multisample,
        );

        // Roads (M23): the opaque twin of the mesh pipeline — its own uniform
        // at group 3, the mesh pass's object, frame and shadow bindings, and
        // the one pipeline in the engine that reads a UV.
        let road_layout = uniform_layout(
            "road-uniforms",
            Some(std::mem::size_of::<RoadUniform>() as u64),
        );
        // A road and a meadow duplicate the mesh shader's lighting, so each
        // receives GI through its own splice rather than through `with_surface`
        // — and the `false` arm is `road.wgsl` and `meadow.wgsl` exactly as they
        // sit on disk, which is what keeps every committed baseline untouched.
        let road_pipeline = Self::road_pipeline(
            cascades,
            device,
            &object_layout,
            &frame_layout,
            &frame_textures_layout,
            &road_layout,
            &vertex_layouts,
            format,
            multisample,
            if gi {
                with_road_gi()
            } else {
                std::borrow::Cow::Borrowed(include_str!("../shaders/road.wgsl"))
            },
        );

        // Meadows (M29): its own uniform, the mesh pass's frame binding, and
        // the shadow map — grass receives shadows even though it casts none.
        let meadow_layout = uniform_layout(
            "meadow-uniforms",
            Some(std::mem::size_of::<MeadowUniform>() as u64),
        );
        let meadow_pipeline = Self::meadow_pipeline(
            cascades,
            device,
            &meadow_layout,
            &frame_layout,
            &frame_textures_layout,
            format,
            multisample,
            if gi {
                with_meadow_gi()
            } else {
                std::borrow::Cow::Borrowed(include_str!("../shaders/meadow.wgsl"))
            },
        );

        let (depth_resolve_pipeline, depth_source_layout) =
            Self::depth_resolve_pipeline(device, samples);

        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/blit.wgsl").into()),
        });
        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit-pipeline-layout"),
            bind_group_layouts: &[Some(&frame_textures_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit-pipeline"),
            layout: Some(&blit_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            // This path only exists when MSAA is off, so the blit is never
            // multisampled.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let (hud_pipeline, hud_layout) = Self::hud_pipeline(device, format);

        let particle_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particle-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/particles.wgsl").into()),
        });
        let particle_layout = uniform_layout("particle-uniforms", None);
        let particle_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("particle-pipeline-layout"),
                bind_group_layouts: &[Some(&particle_layout)],
                immediate_size: 0,
            });

        // One instance per particle; the quad corners come from vertex_index.
        let particle_vertex_layouts = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ParticleRaw>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        })];

        // Two pipelines, one shader, one instance buffer: the *only* difference
        // is the blend equation, and which particle uses which is a CPU-side
        // partition of the sorted draw list.
        //
        // `ALPHA_BLENDING` is `src·srcA + dst·(1-srcA)` — a sprite hides what
        // it covers. Additive is `src·srcA + dst·1` — it only ever adds light,
        // so a stack of flame sprites climbs toward white and the darkest a
        // flame can make anything is "unchanged". Doing this by shipping
        // premultiplied color through one pipeline (emitting alpha 0 for the
        // additive case) would also work and would save a pipeline, but it
        // would move the multiply by alpha from the blend unit into the shader
        // for *every* particle — and rearranging arithmetic that eleven
        // committed baselines depend on, to save one pipeline object, is the
        // wrong trade. Alpha-blended particles keep the exact pipeline they had.
        let particle_pipeline_for = |label: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&particle_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &particle_shader,
                    entry_point: Some("vs_main"),
                    buffers: &particle_vertex_layouts,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &particle_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                // Billboards always face the camera; culling would be a no-op at
                // best and a winding trap at worst.
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                // Depth-test against the meshes but never write: translucent
                // sprites must not occlude each other (they are sorted and
                // blended instead).
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };
        let particle_pipeline =
            particle_pipeline_for("particle-pipeline", wgpu::BlendState::ALPHA_BLENDING);
        let additive_particle_pipeline = particle_pipeline_for(
            "particle-pipeline-additive",
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                // The scene target is opaque, so nothing reads this back; keep
                // it saturating rather than leaving it at whatever the default
                // would imply.
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            },
        );

        let frame_uniform = Uniforms::new(
            device,
            &frame_layout,
            "frame-uniform",
            std::mem::size_of::<FrameUniform>() as u64,
            None,
        );
        let particle_uniform = Uniforms::new(
            device,
            &particle_layout,
            "particle-frame-uniform",
            std::mem::size_of::<ParticleFrameUniform>() as u64,
            None,
        );

        // Dynamic offsets must land on the device's uniform alignment, so the
        // per-object stride is the struct size rounded up to it.
        let alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
        let object_stride =
            std::mem::size_of::<ObjectUniform>().next_multiple_of(alignment as usize) as u64;

        let shadow_placeholder = ShadowMap::new(device, 1, cascades);
        let depth_placeholder = placeholder_texture(
            device,
            "scene-depth-placeholder",
            wgpu::TextureFormat::R32Float,
        );
        let color_placeholder = placeholder_texture(device, "scene-color-placeholder", format);
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow-sampler"),
            // Linear filtering on a comparison sampler is hardware PCF: each
            // tap already returns a bilinear blend of four depth *tests*, so
            // the 3×3 kernel in the shader is effectively 6×6 for free.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let scene_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("scene-color-sampler"),
            // Clamped, because a refraction offset that runs off the frame has
            // no data to read and the honest failure is a stretched edge.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let water_stride =
            std::mem::size_of::<WaterUniform>().next_multiple_of(alignment as usize) as u64;
        let cloud_stride =
            std::mem::size_of::<CloudUniform>().next_multiple_of(alignment as usize) as u64;
        let road_stride =
            std::mem::size_of::<RoadUniform>().next_multiple_of(alignment as usize) as u64;
        let palette_stride =
            std::mem::size_of::<JointPaletteUniform>().next_multiple_of(alignment as usize) as u64;
        let meadow_stride =
            std::mem::size_of::<MeadowUniform>().next_multiple_of(alignment as usize) as u64;

        // Before the struct literal, which moves `frame_layout` into it.
        let cascade_resources =
            (cascades > 1).then(|| CascadeResources::new(device, &frame_layout, cascades));

        Self {
            pipeline,
            transparent_pipeline,
            refractive_pipeline,
            terrain_pipeline,
            textured_pipeline,
            textured_transparent_pipeline,
            material_layout,
            map_sampler,
            shadow_pipeline,
            shadow_cutout_pipeline,
            sky_pipeline,
            water_pipeline,
            refractive_water_pipeline,
            cloud_pipeline,
            road_pipeline,
            road_layout,
            road_objects: None,
            road_stride,
            skinned: None,
            foliage: None,
            skinned_objects: None,
            palette_stride,
            meadow_pipeline,
            meadow_layout,
            meadow_objects: None,
            meadow_stride,
            meadow_meshes: HashMap::new(),
            depth_resolve_pipeline,
            blit_pipeline,
            water_layout,
            cloud_layout,
            depth_source_layout,
            particle_pipeline,
            additive_particle_pipeline,
            object_layout,
            frame_layout,
            skinned_object_layout,
            hud_pipeline,
            hud_layout,
            frame_textures_layout,
            gi,
            gi_field: None,
            gi_placeholder: gi_placeholder_view(device),
            shadow_sampler,
            scene_sampler,
            format,
            samples,
            cascades,
            cascade_resources,
            shadow_placeholder,
            depth_placeholder,
            color_placeholder,
            frame_textures: None,
            shadow_map: None,
            meshes: HashMap::new(),
            textures: HashMap::new(),
            materials: HashMap::new(),
            white_texture: None,
            frame_uniform,
            objects: None,
            object_stride,
            water_objects: None,
            water_stride,
            cloud_objects: None,
            cloud_stride,
            scene_depth: None,
            scene_color: None,
            depth_source: None,
            particle_instances: None,
            particle_uniform,
            hud_targets: Vec::new(),
            frame_index: 0,
        }
    }

    /// Build the skinned pipeline set, once, on the first frame that has a
    /// skinned draw (M27).
    ///
    /// Six shader modules, which is why this is lazy rather than part of the
    /// constructor: every `engine screenshot` in this repo but one has no
    /// skinned mesh in it and should not pay for compiling them. The precedent
    /// is the shadow map, the 1×1 white texture, and the colour copy — all
    /// allocated by the first frame that needs them.
    pub(crate) fn build_skinned(&self, device: &wgpu::Device) -> SkinnedPipelines {
        let multisample = wgpu::MultisampleState {
            count: self.samples,
            ..Default::default()
        };
        let module = |label: &str, source: std::borrow::Cow<'static, str>| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_cascades(
                    &source,
                    self.cascades,
                ))),
            })
        };
        // The renderer's own flag, not a parameter: these are built lazily, on
        // the first skinned draw, long after the constructor that decided it.
        let gi = self.gi;
        let plain = module("skinned-shader", with_gi(vec![skin_producer()], gi));
        let textured = module(
            "skinned-textured-shader",
            with_gi(vec![skin_producer(), texture_producer()], gi),
        );
        let refractive = module(
            "skinned-refractive-shader",
            with_gi(vec![skin_producer(), refraction_producer()], gi),
        );
        let textured_blended = module(
            "skinned-textured-blended-shader",
            with_gi(
                vec![skin_producer(), texture_producer(), refraction_producer()],
                gi,
            ),
        );

        // Position, normal, UV, joints, weights. The joints arrive as
        // `Uint16x4` — a 16-bit index is what glTF writes and what 128 joints
        // need — and land in the shader as `vec4<u32>`.
        let joints = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 3,
                format: wgpu::VertexFormat::Uint16x4,
            }],
        };
        let weights = wgpu::VertexBufferLayout {
            array_stride: 16,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 4,
                format: wgpu::VertexFormat::Float32x4,
            }],
        };
        let position = wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            }],
        };
        let normal = wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x3,
            }],
        };
        let uv = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x2,
            }],
        };

        // Untextured skinning skips the UV slot entirely rather than binding a
        // padded one: the shader does not declare `@location(2)`, and a layout
        // that provides an attribute the stage never reads is a mismatch worth
        // not relying on.
        let plain_layouts = [
            Some(position.clone()),
            Some(normal.clone()),
            Some(joints.clone()),
            Some(weights.clone()),
        ];
        let textured_layouts = [
            Some(position.clone()),
            Some(normal.clone()),
            Some(uv.clone()),
            Some(joints.clone()),
            Some(weights.clone()),
        ];
        let caster_layouts = [Some(position), Some(joints), Some(weights)];

        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("skinned-pipeline-layout"),
            bind_group_layouts: &[
                Some(&self.skinned_object_layout),
                Some(&self.frame_layout),
                Some(&self.frame_textures_layout),
            ],
            immediate_size: 0,
        });
        let material_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("skinned-textured-pipeline-layout"),
                bind_group_layouts: &[
                    Some(&self.skinned_object_layout),
                    Some(&self.frame_layout),
                    Some(&self.frame_textures_layout),
                    Some(&self.material_layout),
                ],
                immediate_size: 0,
            });

        let format = self.format;
        let mesh_pipeline = |label: &str,
                             module: &wgpu::ShaderModule,
                             layout: &wgpu::PipelineLayout,
                             buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                             blend: wgpu::BlendState,
                             depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };

        let caster = |label: &str,
                      module: &wgpu::ShaderModule,
                      layout: &wgpu::PipelineLayout,
                      buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                      fragment: Option<wgpu::FragmentState<'_>>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // Front-face culled, exactly like the unskinned casters:
                    // the map should record each caster's far side.
                    cull_mode: Some(wgpu::Face::Front),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: wgpu::DepthBiasState {
                        constant: 2,
                        slope_scale: 2.0,
                        clamp: 0.0,
                    },
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let shadow_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("skinned-shadow-shader"),
            source: wgpu::ShaderSource::Wgsl(skinned_shadow().into()),
        });
        let shadow_cutout_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("skinned-shadow-cutout-shader"),
            source: wgpu::ShaderSource::Wgsl(skinned_shadow_cutout().into()),
        });
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("skinned-shadow-pipeline-layout"),
            bind_group_layouts: &[Some(&self.skinned_object_layout), Some(&self.frame_layout)],
            immediate_size: 0,
        });
        let shadow_cutout_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("skinned-shadow-cutout-pipeline-layout"),
            bind_group_layouts: &[
                Some(&self.skinned_object_layout),
                Some(&self.frame_layout),
                // The material group at 2, not 3: this pipeline has no frame
                // textures to read — it *is* what writes one of them.
                Some(&self.material_layout),
            ],
            immediate_size: 0,
        });

        SkinnedPipelines {
            opaque: mesh_pipeline(
                "skinned-pipeline",
                &plain,
                &mesh_layout,
                &plain_layouts,
                wgpu::BlendState::REPLACE,
                true,
            ),
            textured: mesh_pipeline(
                "skinned-textured-pipeline",
                &textured,
                &material_pipeline_layout,
                &textured_layouts,
                wgpu::BlendState::REPLACE,
                true,
            ),
            transparent: mesh_pipeline(
                "skinned-transparent-pipeline",
                &plain,
                &mesh_layout,
                &plain_layouts,
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                false,
            ),
            textured_transparent: mesh_pipeline(
                "skinned-textured-transparent-pipeline",
                &textured_blended,
                &material_pipeline_layout,
                &textured_layouts,
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                false,
            ),
            refractive: mesh_pipeline(
                "skinned-refractive-pipeline",
                &refractive,
                &mesh_layout,
                &plain_layouts,
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                false,
            ),
            shadow: caster(
                "skinned-shadow-pipeline",
                &shadow_module,
                &shadow_layout,
                &caster_layouts,
                None,
            ),
            shadow_cutout: caster(
                "skinned-shadow-cutout-pipeline",
                &shadow_cutout_module,
                &shadow_cutout_layout,
                &textured_layouts,
                Some(wgpu::FragmentState {
                    module: &shadow_cutout_module,
                    entry_point: Some("fs_main"),
                    targets: &[],
                    compilation_options: Default::default(),
                }),
            ),
        }
    }

    /// Build the foliage pipeline set, once, on the first frame that has a tree
    /// that moves (M46).
    ///
    /// The bind group layouts are the *ordinary* ones — foliage needs no group
    /// of its own, unlike skinning with its palette, because the wind is four
    /// lanes of the object uniform every draw already has. What it does need is
    /// one more vertex slot, at location 5: 3 and 4 belong to skinning, and a
    /// tree is never skinned, but a shared location that only works because
    /// nothing composes them is the kind of coincidence this repo writes down
    /// rather than relies on.
    pub(crate) fn build_foliage(&self, device: &wgpu::Device) -> FoliagePipelines {
        let multisample = wgpu::MultisampleState {
            count: self.samples,
            ..Default::default()
        };
        let module = |label: &str, source: std::borrow::Cow<'static, str>| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_cascades(
                    &source,
                    self.cascades,
                ))),
            })
        };
        let gi = self.gi;
        let plain = module("foliage-shader", with_foliage(gi));
        let textured = module("foliage-textured-shader", with_foliage_textures(gi));

        let position = wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            }],
        };
        let normal = wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x3,
            }],
        };
        let uv = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x2,
            }],
        };
        // x = the wind weight, y = the leaf's flutter phase in turns.
        let sway = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 5,
                format: wgpu::VertexFormat::Float32x2,
            }],
        };

        let plain_layouts = [
            Some(position.clone()),
            Some(normal.clone()),
            Some(sway.clone()),
        ];
        let textured_layouts = [
            Some(position.clone()),
            Some(normal.clone()),
            Some(uv.clone()),
            Some(sway.clone()),
        ];
        // The solid caster carries a normal it would otherwise not need: the
        // flutter displaces along it, and a caster that disagreed with the
        // colour pass about where a leaf is would write acne under every leaf.
        let caster_layouts = [Some(position), Some(normal), Some(sway.clone())];
        let cutout_caster_layouts = [
            plain_layouts[0].clone(),
            plain_layouts[1].clone(),
            Some(uv),
            Some(sway),
        ];

        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("foliage-pipeline-layout"),
            bind_group_layouts: &[
                Some(&self.object_layout),
                Some(&self.frame_layout),
                Some(&self.frame_textures_layout),
            ],
            immediate_size: 0,
        });
        let material_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("foliage-textured-pipeline-layout"),
                bind_group_layouts: &[
                    Some(&self.object_layout),
                    Some(&self.frame_layout),
                    Some(&self.frame_textures_layout),
                    Some(&self.material_layout),
                ],
                immediate_size: 0,
            });

        let format = self.format;
        let mesh_pipeline = |label: &str,
                             module: &wgpu::ShaderModule,
                             layout: &wgpu::PipelineLayout,
                             buffers: &[Option<wgpu::VertexBufferLayout<'_>>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };

        let shadow_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("foliage-shadow-shader"),
            source: wgpu::ShaderSource::Wgsl(foliage_shadow().into()),
        });
        let shadow_cutout_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("foliage-shadow-cutout-shader"),
            source: wgpu::ShaderSource::Wgsl(foliage_shadow_cutout().into()),
        });
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("foliage-shadow-pipeline-layout"),
            bind_group_layouts: &[Some(&self.object_layout), Some(&self.frame_layout)],
            immediate_size: 0,
        });
        let shadow_cutout_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("foliage-shadow-cutout-pipeline-layout"),
            bind_group_layouts: &[
                Some(&self.object_layout),
                Some(&self.frame_layout),
                // The material group at 2, not 3 — this pipeline has no frame
                // textures to read.
                Some(&self.material_layout),
            ],
            immediate_size: 0,
        });
        let caster = |label: &str,
                      module: &wgpu::ShaderModule,
                      layout: &wgpu::PipelineLayout,
                      buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                      fragment: Option<wgpu::FragmentState<'_>>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: Some(wgpu::Face::Front),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: wgpu::DepthBiasState {
                        constant: 2,
                        slope_scale: 2.0,
                        clamp: 0.0,
                    },
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        FoliagePipelines {
            opaque: mesh_pipeline("foliage-pipeline", &plain, &mesh_layout, &plain_layouts),
            textured: mesh_pipeline(
                "foliage-textured-pipeline",
                &textured,
                &material_pipeline_layout,
                &textured_layouts,
            ),
            shadow: caster(
                "foliage-shadow-pipeline",
                &shadow_module,
                &shadow_layout,
                &caster_layouts,
                None,
            ),
            shadow_cutout: caster(
                "foliage-shadow-cutout-pipeline",
                &shadow_cutout_module,
                &shadow_cutout_layout,
                &cutout_caster_layouts,
                Some(wgpu::FragmentState {
                    module: &shadow_cutout_module,
                    entry_point: Some("fs_main"),
                    targets: &[],
                    compilation_options: Default::default(),
                }),
            ),
        }
    }

    /// The depth-only caster pass (M16). No fragment stage and no color
    /// target: the rasterizer writing depth is the whole point.
    ///
    /// Culling is inverted relative to the mesh pass. Recording the *back* of
    /// each caster moves the stored depth away from the lit surface by the
    /// thickness of the object, which is a far better peeling margin than any
    /// constant bias, and it costs nothing.
    pub(crate) fn shadow_pipeline(
        device: &wgpu::Device,
        object_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/shadow.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow-pipeline-layout"),
            bind_group_layouts: &[Some(object_layout), Some(frame_layout)],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: vertex_layouts,
                compilation_options: Default::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Front),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                // A slope-scaled hardware bias on top of the shader's, which
                // is what keeps large ground-facing polygons from self-
                // shadowing in bands.
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    }

    /// The caster pass for alpha-cut materials (M26).
    ///
    /// Identical to [`Self::shadow_pipeline`] in every state that matters —
    /// front-face culled, the same slope-scaled bias, no colour target — and
    /// different only in having a fragment stage at all. Two pipelines rather
    /// than one with a branch, so the depth-only pass every current scene casts
    /// through is the one it always was.
    pub(crate) fn shadow_cutout_pipeline(
        device: &wgpu::Device,
        object_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        material_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow-cutout-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/shadow_cutout.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow-cutout-pipeline-layout"),
            bind_group_layouts: &[
                Some(object_layout),
                Some(frame_layout),
                Some(material_layout),
            ],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow-cutout-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: vertex_layouts,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // A cut-out card is a *sheet*: culling its front faces the way
                // solid casters are culled would delete it from the map
                // entirely whenever the sun is on its front side, and the
                // peeling margin that trick buys is meaningless on geometry
                // with no thickness.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    }

    /// The procedural sky (M16): one fullscreen triangle, drawn before the
    /// meshes with the depth test passing always and depth writes off, so
    /// every mesh that follows simply covers it.
    pub(crate) fn sky_pipeline(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(include_str!("../shaders/sky.wgsl"))),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky-pipeline-layout"),
            bind_group_layouts: &[Some(frame_layout)],
            immediate_size: 0,
        });

        recipe_pipeline(
            device,
            format,
            multisample,
            RecipePipeline {
                label: "sky-pipeline",
                shader: &shader,
                layout: &layout,
                buffers: &[],
                blend: wgpu::BlendState::REPLACE,
                cull_mode: None,
                depth_write: false,
                // The sky pass draws behind everything by construction — it
                // runs first and never tests what it cannot occlude.
                depth_compare: wgpu::CompareFunction::Always,
            },
        )
    }

    /// The water pass (M18): the blended twin of the mesh pipeline in how it
    /// composites, and nothing like it in what it draws.
    ///
    /// Two departures worth naming. It is **not culled**, because a water
    /// surface is a single sheet with no inside: back-face culling would delete
    /// it the moment a camera dipped below the waterline, and the fragment
    /// shader flips the normal toward the viewer instead. And like the
    /// transparent mesh pipeline it tests depth without writing it — two water
    /// surfaces at different heights have to blend, and a surface that wrote
    /// depth would also occlude the particles of its own spray.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn water_pipeline(
        device: &wgpu::Device,
        water_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        frame_textures_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
        // The M18 file as it sits on disk, or the M27 variant with refraction
        // spliced in. Passed rather than branched on, so the plain pipeline's
        // source is `include_str!` and nothing else.
        source: std::borrow::Cow<'static, str>,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("water-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&source)),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("water-pipeline-layout"),
            bind_group_layouts: &[
                Some(water_layout),
                Some(frame_layout),
                Some(frame_textures_layout),
            ],
            immediate_size: 0,
        });

        recipe_pipeline(
            device,
            format,
            multisample,
            RecipePipeline {
                label: "water-pipeline",
                shader: &shader,
                layout: &layout,
                buffers: vertex_layouts,
                blend: wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                cull_mode: None,
                depth_write: false,
                depth_compare: wgpu::CompareFunction::Less,
            },
        )
    }

    /// The cloud pass (M20): blended like the water pass, and culled like
    /// neither of the others.
    ///
    /// **Culling is off**, and that is load-bearing twice over. A cloud has no
    /// inside, so back-face culling would delete it the instant the camera flew
    /// into one; and the far wall of every lobe is what the near wall is being
    /// blended *over*, which is the accumulation standing in for thickness.
    ///
    /// Depth is tested but never written, like every other blended thing here.
    /// Two clouds have to blend rather than the nearer one masking the farther,
    /// and a cloud that wrote depth would occlude the sky's own reflection in
    /// the water below it.
    pub(crate) fn cloud_pipeline(
        device: &wgpu::Device,
        cloud_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cloud-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(include_str!(
                "../shaders/clouds.wgsl"
            ))),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud-pipeline-layout"),
            bind_group_layouts: &[Some(cloud_layout), Some(frame_layout)],
            immediate_size: 0,
        });

        recipe_pipeline(
            device,
            format,
            multisample,
            RecipePipeline {
                label: "cloud-pipeline",
                shader: &shader,
                layout: &layout,
                buffers: vertex_layouts,
                blend: wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                cull_mode: None,
                depth_write: false,
                depth_compare: wgpu::CompareFunction::Less,
            },
        )
    }

    /// The road pass (M23): the mesh pipeline's opaque twin, with a fourth
    /// bind group for the marking parameters and a third vertex slot for the
    /// surface coordinates they are painted in.
    ///
    /// Everything else matches the mesh pipeline exactly — back-face culled,
    /// depth-tested and depth-writing, `REPLACE` blending — because a road is
    /// ordinary opaque geometry. It is a separate pipeline for the shader's
    /// sake, not the state's.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn road_pipeline(
        cascades: u32,
        device: &wgpu::Device,
        object_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        frame_textures_layout: &wgpu::BindGroupLayout,
        road_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
        // The water pipeline's shape (M27): the caller hands in the source, so
        // this constructor never has to know which variant it is building.
        source: std::borrow::Cow<'static, str>,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("road-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_cascades(&source, cascades))),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("road-pipeline-layout"),
            bind_group_layouts: &[
                Some(object_layout),
                Some(frame_layout),
                Some(frame_textures_layout),
                Some(road_layout),
            ],
            immediate_size: 0,
        });

        recipe_pipeline(
            device,
            format,
            multisample,
            RecipePipeline {
                label: "road-pipeline",
                shader: &shader,
                layout: &layout,
                buffers: vertex_layouts,
                blend: wgpu::BlendState::REPLACE,
                cull_mode: Some(wgpu::Face::Back),
                depth_write: true,
                depth_compare: wgpu::CompareFunction::Less,
            },
        )
    }

    /// The meadow pass (M29): opaque, depth-writing, and **instanced**.
    ///
    /// Two vertex buffers rather than the mesh pass's three, and both are this
    /// pipeline's own: a plant template with channels no `MeshData` has, and a
    /// per-instance record of where each copy of it stands. That is why the
    /// layout is interleaved here where every other pipeline's is split — a
    /// meadow does not share the geometry cache, so there is no mesh with no
    /// normals to keep unpadded.
    ///
    /// **Culling is off**, and it is load-bearing: a blade of grass is a
    /// single-sided strip, half of every tuft faces away from any given camera,
    /// and back-face culling would delete it. The alternative — emitting both
    /// faces — doubles the template for nothing, since the fragment stage can
    /// flip the normal toward the viewer in one line. `clouds.wgsl` set this
    /// precedent for its own reason.
    ///
    /// There is no shadow-caster twin. See `meadow.wgsl`'s header.
    // Eight, since M35 gave this the `source` parameter `road_pipeline` has had
    // since M27 — the same list as its neighbour above, which carries the same
    // allow for the same reason.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn meadow_pipeline(
        cascades: u32,
        device: &wgpu::Device,
        meadow_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        frame_textures_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
        source: std::borrow::Cow<'static, str>,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("meadow-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_cascades(&source, cascades))),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("meadow-pipeline-layout"),
            bind_group_layouts: &[
                Some(meadow_layout),
                Some(frame_layout),
                Some(frame_textures_layout),
            ],
            immediate_size: 0,
        });

        // `MeadowVertex`: centre, normal, offset, anchor, span, organ.
        const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x3,
            2 => Float32x3,
            3 => Float32x3,
            4 => Float32x3,
            5 => Uint32,
        ];
        // `MeadowInstance`: position+scale, yaw+phase+gradient, seed.
        const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
            6 => Float32x4,
            7 => Float32x4,
            8 => Uint32,
        ];

        recipe_pipeline(
            device,
            format,
            multisample,
            RecipePipeline {
                label: "meadow-pipeline",
                shader: &shader,
                layout: &layout,
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<MeadowVertexRaw>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &VERTEX_ATTRIBUTES,
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<MeadowInstanceRaw>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &INSTANCE_ATTRIBUTES,
                    }),
                ],
                blend: wgpu::BlendState::REPLACE,
                cull_mode: None,
                depth_write: true,
                depth_compare: wgpu::CompareFunction::Less,
            },
        )
    }

    /// The depth copy pass (M18): one fullscreen triangle turning the opaque
    /// pass's depth attachment into something the water shader can read.
    ///
    /// The source's binding type must match its sample count, and the shader
    /// text is patched accordingly — which is fine here because the sample count
    /// is baked into the renderer already (`with_samples`).
    pub(crate) fn depth_resolve_pipeline(
        device: &wgpu::Device,
        samples: u32,
    ) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
        let multisampled = samples > 1;
        let source_type = if multisampled {
            "texture_depth_multisampled_2d"
        } else {
            "texture_depth_2d"
        };
        let source = include_str!("../shaders/depth_resolve.wgsl")
            .replace("SOURCE_TEXTURE_TYPE", source_type);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("depth-resolve-shader"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let source_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("depth-resolve-source"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("depth-resolve-layout"),
            bind_group_layouts: &[Some(&source_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("depth-resolve-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::RED,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            // The copy target is single-sampled however many samples the scene
            // draws with, so this pass is never multisampled.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        (pipeline, source_layout)
    }

    /// The HUD overlay blit (M12): fullscreen triangle, no vertex buffers, no
    /// sampler (`textureLoad` — the canvas is 1:1 with target pixels, so
    /// nothing filters a glyph edge), straight-alpha blend over the lit scene.
    /// The canvas covers only the region the HUD touches, so the fetch is
    /// offset by that region's corner and a scissor rect bounds the triangle.
    pub(crate) fn hud_pipeline(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hud-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/hud.wgsl").into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hud-overlay"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hud-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hud-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Straight alpha: the canvas stores unpremultiplied color,
                    // so alpha 1 replaces the scene byte exactly and alpha 0
                    // leaves it exactly.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        (pipeline, layout)
    }
}

impl SkinnedObjects {
    /// Make sure the palette buffer holds `size` bytes and that the group-0
    /// bind group names it beside the current object buffer.
    ///
    /// Rebuilt only when one of the two buffers was reallocated, which the
    /// recorded capacities detect: a bind group holds its buffers by identity,
    /// so a draw list that outgrew the object buffer would otherwise keep
    /// binding the freed one.
    pub(crate) fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        objects: &Uniforms,
        size: u64,
    ) {
        let fits = slot
            .as_ref()
            .is_some_and(|held| held.palette_size >= size && held.objects_size == objects.size);
        if fits {
            return;
        }

        let palette = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("joint-palettes"),
            size: size.max(std::mem::size_of::<JointPaletteUniform>() as u64),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group =
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("skinned-object-uniforms"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &objects.buffer,
                            offset: 0,
                            size: std::num::NonZeroU64::new(
                                std::mem::size_of::<ObjectUniform>() as u64
                            ),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &palette,
                            offset: 0,
                            size: std::num::NonZeroU64::new(
                                std::mem::size_of::<JointPaletteUniform>() as u64,
                            ),
                        }),
                    },
                ],
            });
        *slot = Some(Self {
            palette_size: size,
            objects_size: objects.size,
            palette,
            bind_group,
        });
    }
}
