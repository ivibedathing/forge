//! GPU device acquisition, independent of whether we are drawing to a window.
//!
//! Kept separate from both the window loop and the renderer because
//! `engine screenshot` (M1) needs a device with no surface at all. Anything
//! that assumes a window exists belongs in `window.rs`, not here.

use engine_core::{EngineError, Result};

/// The formats the renderer attaches to a pass on *every* frame, whatever the
/// scene says, with what each one is for. An adapter that cannot render one of
/// them cannot draw anything this engine draws.
///
/// `R32Float` is the one that actually catches machines. The WebGPU spec
/// guarantees it renderable, so a compliant adapter passes this check for
/// free — but wgpu asks a *downlevel* backend what it really supports rather
/// than trusting the spec, and the software GL stack a Linux CI runner falls
/// back to answers no. Without this check that adapter reports itself
/// available, every render test stops taking its skip path, and the failure
/// they then hit is a wgpu validation panic several calls later reported as
/// `internal_panic` — "this is an engine bug", which is exactly wrong. The
/// adapter is refused here instead, by the code that already means "nothing
/// here can draw", so the skip paths keep working and the message names the
/// adapter and the capability it is missing.
const REQUIRED_ATTACHMENTS: [(wgpu::TextureFormat, &str); 3] = [
    (wgpu::TextureFormat::Rgba8UnormSrgb, "the scene target"),
    (
        wgpu::TextureFormat::Depth32Float,
        "the depth buffer and the shadow map",
    ),
    (
        wgpu::TextureFormat::R32Float,
        "the depth copy the water and refraction passes read",
    ),
];

/// An acquired adapter, device, and queue.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// Acquire a GPU. `compatible_surface` constrains adapter selection when
    /// rendering to a window; pass `None` for headless work.
    pub async fn new(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface,
                // Bucketing exists to stop untrusted content fingerprinting the
                // GPU. This is a local desktop tool; take the real limits.
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| {
                EngineError::new(
                    engine_core::codes::NO_GPU_ADAPTER,
                    format!("no compatible GPU adapter found: {e}"),
                )
            })?;

        for (format, purpose) in REQUIRED_ATTACHMENTS {
            let usages = adapter.get_texture_format_features(format).allowed_usages;
            if !usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
                let info = adapter.get_info();
                return Err(EngineError::new(
                    engine_core::codes::NO_GPU_ADAPTER,
                    format!(
                        "no usable GPU adapter found: {} ({:?}) cannot render to {format:?}, \
                         which this engine needs for {purpose}",
                        info.name, info.backend
                    ),
                ));
            }
        }

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("engine-device"),
                required_features: wgpu::Features::empty(),
                // Stay within the downlevel/WebGL-class limits until a feature
                // actually needs more. Requesting the adapter's full limits
                // makes the engine silently non-portable.
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| {
                EngineError::new(
                    engine_core::codes::DEVICE_REQUEST_FAILED,
                    format!("could not open GPU device: {e}"),
                )
            })?;

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// Create an instance with the platform's default backends.
    ///
    /// Reads `WGPU_BACKEND`, `WGPU_POWER_PREF` and friends from the
    /// environment, which gives an agent a way to force a specific backend
    /// when diagnosing a driver-specific render difference.
    pub fn default_instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
    }

    /// Adapter description, for `engine info` and for error reports.
    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }
}
