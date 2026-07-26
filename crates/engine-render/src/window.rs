//! Windowed presentation: surface setup and swapchain lifecycle.
//!
//! Everything here is window-specific by design. The headless path (M1) goes
//! straight from `Gpu` to `Renderer` and never touches this module.

use std::sync::Arc;

use engine_core::{EngineError, Result};

use crate::{Frame, Gpu, Renderer};

/// A window surface plus the GPU state needed to present to it.
pub struct WindowTarget {
    pub gpu: Gpu,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
}

impl WindowTarget {
    /// Build a surface for `window` and a renderer matching its format.
    ///
    /// Takes an `Arc<Window>` so the surface can borrow the window for
    /// `'static`; wgpu requires the window to outlive the surface, and an Arc
    /// is how that is expressed without unsafe.
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

        // A zero-sized surface is not configurable; callers that may be
        // minimized should skip creation until there is real area.
        let (width, height) = (width.max(1), height.max(1));

        let config = surface
            .get_default_config(&gpu.adapter, width, height)
            .ok_or_else(|| {
                EngineError::new(
                    "surface_unsupported",
                    "the selected adapter cannot present to this window's surface",
                )
            })?;

        surface.configure(&gpu.device, &config);

        let renderer = Renderer::new(&gpu.device, config.format);

        Ok(Self {
            gpu,
            surface,
            config,
            renderer,
        })
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

    /// Draw and present one frame.
    pub fn render(&mut self) -> Result<()> {
        use wgpu::CurrentSurfaceTexture as Acquired;

        // `suboptimal` defers reconfiguration until after present: configuring
        // a surface while one of its textures is still alive panics.
        let (surface_texture, suboptimal) = match self.surface.get_current_texture() {
            Acquired::Success(texture) => (texture, false),

            // Usable this frame, but the swapchain no longer matches the
            // surface. Draw it, then reconfigure.
            Acquired::Suboptimal(texture) => (texture, true),

            // Nothing to draw into right now. Skip the frame; these clear up on
            // their own.
            Acquired::Timeout | Acquired::Occluded => return Ok(()),

            // Recoverable by reconfiguring. No texture is alive here, so it is
            // safe to configure immediately.
            Acquired::Outdated | Acquired::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                return Ok(());
            }

            // A validation error was raised and captured by an error scope.
            // This is a bug in our usage, not a transient condition.
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

        self.renderer.draw(
            &self.gpu.device,
            &self.gpu.queue,
            Frame {
                view: &view,
                clear: wgpu::Color {
                    r: 0.05,
                    g: 0.05,
                    b: 0.07,
                    a: 1.0,
                },
            },
        );

        self.gpu.queue.present(surface_texture);

        if suboptimal {
            self.surface.configure(&self.gpu.device, &self.config);
        }

        Ok(())
    }
}
