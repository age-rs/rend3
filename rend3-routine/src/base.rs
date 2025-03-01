//! Starter RenderGraph that can be easily extended.
//!
//! This is a fully put together pipeline to render with rend3. If you don't
//! need any customization, this should be drop in without worrying about it.
//!
//! In order to start customizing it, copy the contents of
//! [`BaseRenderGraph::add_to_graph`] into your own code and start modifying it.
//! This will allow you to insert your own routines and customize the behavior
//! of the existing routines.
//!
//! [`BaseRenderGraphIntermediateState`] intentionally has all of its members
//! public. If you want to change what rendergraph image things are rendering
//! to, or muck with any of the data in there, you are free to, and the
//! following routines will behave as you configure.

use std::sync::Arc;

use glam::{UVec2, Vec4};
use rend3::{
    graph::{
        self, DataHandle, InstructionEvaluationOutput, RenderGraph, RenderPassTargets, RenderTargetDescriptor,
        RenderTargetHandle, ViewportRect,
    },
    types::{SampleCount, TextureFormat, TextureUsages},
    Renderer, ShaderPreProcessor, INTERNAL_SHADOW_DEPTH_FORMAT,
};
use wgpu::BindGroup;

use crate::{
    clear,
    common::{self, CameraSpecifier},
    forward::{self, ForwardRoutineArgs},
    skinning, uniforms,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct DepthTargets {
    pub single_sample_mipped: RenderTargetHandle,
    pub multi_sample: Option<RenderTargetHandle>,
}

impl DepthTargets {
    pub fn new(graph: &mut RenderGraph<'_>, resolution: UVec2, samples: SampleCount) -> Self {
        let single_sample_mipped = graph.add_render_target(RenderTargetDescriptor {
            label: Some("hdr depth".into()),
            resolution,
            depth: 1,
            mip_levels: None,
            samples: SampleCount::One,
            format: TextureFormat::Depth32Float,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        });

        let multi_sample = samples.needs_resolve().then(|| {
            graph.add_render_target(RenderTargetDescriptor {
                label: Some("hdr depth multisampled".into()),
                resolution,
                depth: 1,
                mip_levels: Some(1),
                samples,
                format: TextureFormat::Depth32Float,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            })
        });

        Self { single_sample_mipped, multi_sample }
    }

    pub fn rendering_target(&self) -> RenderTargetHandle {
        self.multi_sample.unwrap_or(self.single_sample_mipped.set_mips(0..1))
    }
}

pub struct OutputRenderTarget {
    pub handle: RenderTargetHandle,
    pub resolution: UVec2,
    pub samples: SampleCount,
}

pub struct BaseRenderGraphRoutines<'node> {
    pub pbr: &'node crate::pbr::PbrRoutine,
    pub skybox: Option<&'node crate::skybox::SkyboxRoutine>,
    pub tonemapping: &'node crate::tonemapping::TonemappingRoutine,
}

pub struct BaseRenderGraphInputs<'a, 'node> {
    pub eval_output: &'a InstructionEvaluationOutput,
    pub routines: BaseRenderGraphRoutines<'node>,
    pub target: OutputRenderTarget,
}

#[derive(Debug, Default)]
pub struct BaseRenderGraphSettings {
    pub ambient_color: Vec4,
    pub clear_color: Vec4,
}

/// Starter RenderGraph.
///
/// See module for documentation.
pub struct BaseRenderGraph {
    pub interfaces: common::WholeFrameInterfaces,
    pub samplers: common::Samplers,
    pub gpu_skinner: skinning::GpuSkinner,
}

impl BaseRenderGraph {
    pub fn new(renderer: &Arc<Renderer>, spp: &ShaderPreProcessor) -> Self {
        profiling::scope!("DefaultRenderGraphData::new");

        let interfaces = common::WholeFrameInterfaces::new(&renderer.device);

        let samplers = common::Samplers::new(&renderer.device);

        // TODO: Support more materials

        let gpu_skinner = skinning::GpuSkinner::new(&renderer.device, spp);

        Self { interfaces, samplers, gpu_skinner }
    }

    /// Add this to the rendergraph. This is the function you should start
    /// customizing.
    #[allow(clippy::too_many_arguments)]
    pub fn add_to_graph<'node>(
        &'node self,
        graph: &mut RenderGraph<'node>,
        inputs: BaseRenderGraphInputs<'_, 'node>,
        settings: BaseRenderGraphSettings,
    ) {
        // Create the data and handles for the graph.
        let mut state = BaseRenderGraphIntermediateState::new(graph, inputs, settings);

        // Clear the shadow buffers. This, as an explicit node, must be done as a limitation of the graph dependency system.
        state.clear_shadow_buffers();

        // Prepare all the uniforms that all shaders need access to.
        state.create_frame_uniforms(self);

        // Perform compute based skinning.
        state.skinning(self);

        // Render all the shadows to the shadow map.
        state.pbr_shadow_rendering();

        // Do the first pass, rendering the predicted triangles from last frame.
        state.pbr_render();

        // Render the skybox.
        state.skybox();

        // Render all transparent objects.
        //
        // This _must_ happen after culling, as all transparent objects are
        // considered "residual".
        state.pbr_forward_rendering_transparent();

        // Tonemap the HDR inner buffer to the output buffer.
        state.tonemapping();
    }
}

/// Struct that globs all the information the [`BaseRenderGraph`] needs.
///
/// This is intentionally public so all this can be changed by the user if they
/// so desire.
pub struct BaseRenderGraphIntermediateState<'a, 'node> {
    pub graph: &'a mut RenderGraph<'node>,
    pub inputs: BaseRenderGraphInputs<'a, 'node>,
    pub settings: BaseRenderGraphSettings,

    pub shadow_uniform_bg: DataHandle<BindGroup>,
    pub forward_uniform_bg: DataHandle<BindGroup>,

    pub shadow: RenderTargetHandle,
    pub depth: DepthTargets,
    pub primary_renderpass: RenderPassTargets,

    pub pre_skinning_buffers: DataHandle<skinning::PreSkinningBuffers>,
}
impl<'a, 'node> BaseRenderGraphIntermediateState<'a, 'node> {
    /// Create the default setting for all state.
    pub fn new(
        graph: &'a mut RenderGraph<'node>,
        inputs: BaseRenderGraphInputs<'a, 'node>,
        settings: BaseRenderGraphSettings,
    ) -> Self {
        // Create global bind group information
        let shadow_uniform_bg = graph.add_data::<BindGroup>();
        let forward_uniform_bg = graph.add_data::<BindGroup>();

        // Shadow render target
        let shadow = graph.add_render_target(RenderTargetDescriptor {
            label: Some("shadow target".into()),
            resolution: inputs.eval_output.shadow_target_size,
            depth: 1,
            mip_levels: Some(1),
            samples: SampleCount::One,
            format: INTERNAL_SHADOW_DEPTH_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        });

        // Make the actual render targets we want to render to.
        let color = graph.add_render_target(RenderTargetDescriptor {
            label: Some("hdr color".into()),
            resolution: inputs.target.resolution,
            depth: 1,
            samples: inputs.target.samples,
            mip_levels: Some(1),
            format: TextureFormat::Rgba16Float,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        });
        let resolve = inputs.target.samples.needs_resolve().then(|| {
            graph.add_render_target(RenderTargetDescriptor {
                label: Some("hdr resolve".into()),
                resolution: inputs.target.resolution,
                depth: 1,
                mip_levels: Some(1),
                samples: SampleCount::One,
                format: TextureFormat::Rgba16Float,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            })
        });
        let depth = DepthTargets::new(graph, inputs.target.resolution, inputs.target.samples);
        let primary_renderpass = graph::RenderPassTargets {
            targets: vec![graph::RenderPassTarget { color, resolve, clear: settings.clear_color }],
            depth_stencil: Some(graph::RenderPassDepthTarget {
                target: depth.rendering_target(),
                depth_clear: Some(0.0),
                stencil_clear: None,
            }),
        };

        let pre_skinning_buffers = graph.add_data::<skinning::PreSkinningBuffers>();

        Self {
            graph,
            inputs,
            settings,

            shadow_uniform_bg,
            forward_uniform_bg,

            shadow,
            depth,
            primary_renderpass,

            pre_skinning_buffers,
        }
    }

    /// Clear the shadow buffers. This, as an explicit node, must be done as a limitation of the graph dependency system.
    fn clear_shadow_buffers(&mut self) {
        clear::add_depth_clear_to_graph(self.graph, self.shadow, 0.0);
    }

    /// Create all the uniforms all the shaders in this graph need.
    pub fn create_frame_uniforms(&mut self, base: &'node BaseRenderGraph) {
        uniforms::add_to_graph(
            self.graph,
            self.shadow,
            uniforms::UniformBindingHandles {
                interfaces: &base.interfaces,
                shadow_uniform_bg: self.shadow_uniform_bg,
                forward_uniform_bg: self.forward_uniform_bg,
            },
            uniforms::UniformInformation {
                samplers: &base.samplers,
                ambient: self.settings.ambient_color,
                resolution: self.inputs.target.resolution,
            },
        );
    }

    pub fn skinning(&mut self, base: &'node BaseRenderGraph) {
        skinning::add_skinning_to_graph(self.graph, &base.gpu_skinner);
    }

    /// Render all shadows for the PBR materials.
    pub fn pbr_shadow_rendering(&mut self) {
        for (shadow_index, desc) in self.inputs.eval_output.shadows.iter().enumerate() {
            let target = self.shadow.set_viewport(ViewportRect::new(desc.map.offset, UVec2::splat(desc.map.size)));
            let renderpass = graph::RenderPassTargets {
                targets: vec![],
                depth_stencil: Some(graph::RenderPassDepthTarget {
                    target,
                    depth_clear: Some(0.0),
                    stencil_clear: None,
                }),
            };

            let routines = [&self.inputs.routines.pbr.opaque_depth, &self.inputs.routines.pbr.cutout_depth];
            for routine in routines {
                routine.add_forward_to_graph(ForwardRoutineArgs {
                    graph: self.graph,
                    label: &format!("pbr shadow renderering S{shadow_index}"),
                    camera: CameraSpecifier::Shadow(shadow_index as u32),
                    binding_data: forward::ForwardRoutineBindingData {
                        whole_frame_uniform_bg: self.shadow_uniform_bg,
                        per_material_bgl: &self.inputs.routines.pbr.per_material,
                        extra_bgs: None,
                    },
                    samples: SampleCount::One,
                    renderpass: renderpass.clone(),
                });
            }
        }
    }

    /// Render the skybox.
    pub fn skybox(&mut self) {
        if let Some(skybox) = self.inputs.routines.skybox {
            skybox.add_to_graph(
                self.graph,
                self.primary_renderpass.clone(),
                self.forward_uniform_bg,
                self.inputs.target.samples,
            );
        }
    }

    /// Render the PBR materials.
    pub fn pbr_render(&mut self) {
        let routines = [&self.inputs.routines.pbr.opaque_routine, &self.inputs.routines.pbr.cutout_routine];
        for routine in routines {
            routine.add_forward_to_graph(ForwardRoutineArgs {
                graph: self.graph,
                label: "PBR Forward Pass",
                camera: CameraSpecifier::Viewport,
                binding_data: forward::ForwardRoutineBindingData {
                    whole_frame_uniform_bg: self.forward_uniform_bg,
                    per_material_bgl: &self.inputs.routines.pbr.per_material,
                    extra_bgs: None,
                },
                samples: self.inputs.target.samples,
                renderpass: self.primary_renderpass.clone(),
            });
        }
    }

    /// Render the PBR materials.
    pub fn pbr_forward_rendering_transparent(&mut self) {
        self.inputs.routines.pbr.blend_routine.add_forward_to_graph(ForwardRoutineArgs {
            graph: self.graph,
            label: "PBR Forward Transparent",
            camera: CameraSpecifier::Viewport,
            binding_data: forward::ForwardRoutineBindingData {
                whole_frame_uniform_bg: self.forward_uniform_bg,
                per_material_bgl: &self.inputs.routines.pbr.per_material,
                extra_bgs: None,
            },
            samples: self.inputs.target.samples,
            renderpass: self.primary_renderpass.clone(),
        });
    }

    /// Tonemap onto the given render target.
    pub fn tonemapping(&mut self) {
        self.inputs.routines.tonemapping.add_to_graph(
            self.graph,
            self.primary_renderpass.resolved_color(0),
            self.inputs.target.handle,
            self.forward_uniform_bg,
        );
    }
}
