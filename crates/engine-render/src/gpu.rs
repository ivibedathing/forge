//! GPU device acquisition, independent of whether we are drawing to a window.
//!
//! Kept separate from both the window loop and the renderer because
//! `engine screenshot` (M1) needs a device with no surface at all. Anything
//! that assumes a window exists belongs in `window.rs`, not here.

use engine_core::{EngineError, Result};

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
                    "no_gpu_adapter",
                    format!("no compatible GPU adapter found: {e}"),
                )
            })?;

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
                    "device_request_failed",
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
