//! Windowed presentation: surface setup and swapchain lifecycle.
//!
//! Everything here is window-specific by design; the headless path never
//! touches this module. `WindowTarget` owns the surface plumbing and nothing
//! else — what gets drawn each frame is the caller's business, passed in as a
//! closure, so the triangle viewer and the scene viewer share every line of
//! acquire/present handling.

use std::sync::Arc;

use engine_core::{EngineError, Result};

use crate::Gpu;

/// A window surface plus the GPU state needed to present to it.
pub struct WindowTarget {
    pub gpu: Gpu,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
}

impl WindowTarget {
    /// Build a surface for `window`.
    ///
    /// Takes an `Arc<W>` so the surface can borrow the window for `'static`;
    /// wgpu requires the window to outlive the surface, and an Arc is how that
    /// is expressed without unsafe.
    pub fn new<W>(window: Arc<W>, width: u32, height: u32) -> Result<Self>
    where
        W: wgpu::DisplayAndWindowHandle + 'static,
    {
        let instance = Gpu::default_instance();

        let surface = instance.create_surface(window).map_err(|e| {
            EngineError::new(
                "surface_creation_failed",
                format!("could not create a render surface for the window: {e}"),
            )
        })?;

        let gpu = pollster::block_on(Gpu::new(instance, Some(&surface)))?;

        // A zero-sized surface is not configurable; clamp rather than fail.
        let (width, height) = (width.max(1), height.max(1));

        let mut config = surface
            .get_default_config(&gpu.adapter, width, height)
            .ok_or_else(|| {
                EngineError::new(
                    "surface_unsupported",
                    "the selected adapter cannot present to this window's surface",
                )
            })?;

        // Prefer an sRGB swapchain so the window and `engine screenshot` agree
        // (M4 renders in linear space and lets the target encode). If the
        // surface offers no sRGB format, the default stands — a slightly dark
        // viewer beats no viewer.
        if !config.format.is_srgb() {
            let capabilities = surface.get_capabilities(&gpu.adapter);
            if let Some(srgb) = capabilities.formats.iter().copied().find(|f| f.is_srgb()) {
                config.format = srgb;
            }
        }

        surface.configure(&gpu.device, &config);

        Ok(Self {
            gpu,
            surface,
            config,
        })
    }

    /// The swapchain's texture format; pipelines must be built for this.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            // Minimized. Reconfiguring at zero size is invalid; hold the last
            // good config and wait for a real size.
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
    }

    /// Acquire a frame, hand it to `draw`, and present it.
    ///
    /// Transient acquisition states (occluded, outdated, lost…) are absorbed
    /// here: the frame is skipped or the surface reconfigured, and `draw`
    /// simply is not called. Only unrecoverable states become errors.
    pub fn render_with(
        &mut self,
        draw: impl FnOnce(&wgpu::Device, &wgpu::Queue, &wgpu::TextureView),
    ) -> Result<()> {
        use wgpu::CurrentSurfaceTexture as Acquired;

        // `suboptimal` defers reconfiguration until after present: configuring
        // a surface while one of its textures is still alive panics.
        let (surface_texture, suboptimal) = match self.surface.get_current_texture() {
            Acquired::Success(texture) => (texture, false),
            Acquired::Suboptimal(texture) => (texture, true),

            // Nothing to draw into right now; these clear up on their own.
            Acquired::Timeout | Acquired::Occluded => return Ok(()),

            // Recoverable by reconfiguring. No texture is alive here, so it is
            // safe to configure immediately.
            Acquired::Outdated | Acquired::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                return Ok(());
            }

            Acquired::Validation => {
                return Err(EngineError::new(
                    "surface_validation_error",
                    "acquiring a surface frame raised a validation error",
                ));
            }
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        draw(&self.gpu.device, &self.gpu.queue, &view);

        self.gpu.queue.present(surface_texture);

        if suboptimal {
            self.surface.configure(&self.gpu.device, &self.config);
        }

        Ok(())
    }
}
