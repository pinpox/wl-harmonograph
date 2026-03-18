//! Animated harmonograph wallpaper for Sway/Wayland.
//!
//! GPU-accelerated rendering using EGL + OpenGL ES 2.0 on top of
//! smithay-client-toolkit with wlr-layer-shell. The GPU draws anti-aliased
//! curve segments into a framebuffer object (FBO) that accumulates over time.
//! Each frame the CPU only computes 3 pendulum positions and submits 3
//! triangle-strip draw calls — all rasterization happens on the GPU.

mod harmonograph;

use std::env;
use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use glow::HasContext;
use harmonograph::Harmonograph;
use log::info;
use rand::Rng;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    registry_handlers,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_surface};
use wayland_client::{Connection, Proxy, QueueHandle};

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

type Color = (f64, f64, f64);

fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f64 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f64 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f64 / 255.0;
    Some((r, g, b))
}

fn colors_from_env() -> (Vec<Color>, Color) {
    let default_fg: Vec<Color> = vec![
        (0.984, 0.286, 0.204),
        (0.596, 0.592, 0.102),
        (0.988, 0.694, 0.349),
        (0.514, 0.647, 0.596),
        (0.827, 0.525, 0.608),
        (0.557, 0.753, 0.486),
        (0.894, 0.827, 0.529),
    ];
    let default_bg: Color = (0.114, 0.122, 0.137);

    let fg = env::var("HARMONOGRAPH_FG")
        .ok()
        .and_then(|s| {
            let c: Vec<Color> = s.split(',').filter_map(parse_hex_color).collect();
            if c.is_empty() {
                None
            } else {
                Some(c)
            }
        })
        .unwrap_or(default_fg);

    let bg = env::var("HARMONOGRAPH_BG")
        .ok()
        .and_then(|s| parse_hex_color(&s))
        .unwrap_or(default_bg);

    (fg, bg)
}

// ---------------------------------------------------------------------------
// GL renderer
// ---------------------------------------------------------------------------

struct GlRenderer {
    gl: glow::Context,
    program: glow::Program,
    vbo: glow::Buffer,
    fbo: glow::Framebuffer,
    fbo_texture: glow::Texture,
    blit_program: glow::Program,
    blit_vbo: glow::Buffer,
    u_color: glow::UniformLocation,
    u_bg: glow::UniformLocation,
    a_pos: u32,
    a_cross: u32,
    width: u32,
    height: u32,
}

impl GlRenderer {
    unsafe fn new(gl: glow::Context, width: u32, height: u32) -> Self {
        // --- Line drawing shader with edge antialiasing ---
        let vs_src = r#"#version 100
            attribute vec2 a_pos;
            attribute float a_cross;
            varying float v_cross;
            void main() {
                v_cross = a_cross;
                gl_Position = vec4(a_pos, 0.0, 1.0);
            }
        "#;
        let fs_src = r#"#version 100
            precision mediump float;
            uniform vec4 u_color;
            varying float v_cross;
            void main() {
                float d = abs(v_cross);
                float alpha = 1.0 - smoothstep(0.5, 1.0, d);
                gl_FragColor = vec4(u_color.rgb, u_color.a * alpha);
            }
        "#;
        let program = Self::create_program(&gl, vs_src, fs_src);
        let u_color = gl.get_uniform_location(program, "u_color").unwrap();
        let a_pos = gl.get_attrib_location(program, "a_pos").unwrap();
        let a_cross = gl.get_attrib_location(program, "a_cross").unwrap();

        // --- Blit shader (composite FBO over background color) ---
        let blit_vs = r#"#version 100
            attribute vec2 a_pos;
            varying vec2 v_uv;
            void main() {
                v_uv = a_pos * 0.5 + 0.5;
                gl_Position = vec4(a_pos, 0.0, 1.0);
            }
        "#;
        let blit_fs = r#"#version 100
            precision mediump float;
            varying vec2 v_uv;
            uniform sampler2D u_tex;
            uniform vec3 u_bg;
            void main() {
                vec4 texel = texture2D(u_tex, v_uv);
                gl_FragColor = vec4(mix(u_bg, texel.rgb, texel.a), 1.0);
            }
        "#;
        let blit_program = Self::create_program(&gl, blit_vs, blit_fs);
        let u_bg = gl.get_uniform_location(blit_program, "u_bg").unwrap();

        // VBO for line strips
        let vbo = gl.create_buffer().unwrap();

        // Fullscreen quad VBO for blit
        let blit_vbo = gl.create_buffer().unwrap();
        #[rustfmt::skip]
        let quad: [f32; 12] = [
            -1.0, -1.0,  1.0, -1.0, -1.0,  1.0,
            -1.0,  1.0,  1.0, -1.0,  1.0,  1.0,
        ];
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(blit_vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, cast_f32_slice(&quad), glow::STATIC_DRAW);

        // Create FBO + texture
        let (fbo, fbo_texture) = Self::create_fbo(&gl, width, height);

        // Clear FBO to fully transparent
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.viewport(0, 0, width as i32, height as i32);
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);

        Self {
            gl,
            program,
            vbo,
            fbo,
            fbo_texture,
            blit_program,
            blit_vbo,
            u_color,
            u_bg,
            a_pos,
            a_cross,
            width,
            height,
        }
    }

    unsafe fn create_program(gl: &glow::Context, vs_src: &str, fs_src: &str) -> glow::Program {
        let program = gl.create_program().unwrap();
        let vs = gl.create_shader(glow::VERTEX_SHADER).unwrap();
        gl.shader_source(vs, vs_src);
        gl.compile_shader(vs);
        assert!(
            gl.get_shader_compile_status(vs),
            "VS: {}",
            gl.get_shader_info_log(vs)
        );
        let fs = gl.create_shader(glow::FRAGMENT_SHADER).unwrap();
        gl.shader_source(fs, fs_src);
        gl.compile_shader(fs);
        assert!(
            gl.get_shader_compile_status(fs),
            "FS: {}",
            gl.get_shader_info_log(fs)
        );
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.link_program(program);
        assert!(
            gl.get_program_link_status(program),
            "Link: {}",
            gl.get_program_info_log(program)
        );
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        program
    }

    unsafe fn create_fbo(
        gl: &glow::Context,
        width: u32,
        height: u32,
    ) -> (glow::Framebuffer, glow::Texture) {
        let tex = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            width as i32,
            height as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );

        let fbo = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex),
            0,
        );
        assert_eq!(
            gl.check_framebuffer_status(glow::FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE
        );
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        (fbo, tex)
    }

    /// Reduce the alpha of every pixel in the FBO, keeping RGB intact.
    /// This makes older lines become more transparent each frame while
    /// preserving their original color/saturation.
    unsafe fn fade(&self, fade_amount: f32) {
        let gl = &self.gl;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
        gl.viewport(0, 0, self.width as i32, self.height as i32);

        gl.enable(glow::BLEND);
        // Color: keep unchanged (dst * 1)
        // Alpha: multiply by (1 - fade_amount) via dst * src_alpha
        gl.blend_func_separate(
            glow::ZERO,
            glow::ONE,
            glow::ZERO,
            glow::ONE_MINUS_SRC_ALPHA,
        );

        gl.use_program(Some(self.program));
        gl.uniform_4_f32(Some(&self.u_color), 0.0, 0.0, 0.0, fade_amount);

        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.blit_vbo));
        gl.enable_vertex_attrib_array(self.a_pos);
        gl.vertex_attrib_pointer_f32(self.a_pos, 2, glow::FLOAT, false, 8, 0);
        gl.disable_vertex_attrib_array(self.a_cross);
        gl.vertex_attrib_1_f32(self.a_cross, 0.0);

        gl.draw_arrays(glow::TRIANGLES, 0, 6);

        gl.disable_vertex_attrib_array(self.a_pos);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
    }

    /// Draw a triangle strip (the thickened curve segment) into the FBO.
    /// Vertices are packed as [x, y, cross] where cross is -1.0 or +1.0
    /// indicating which side of the line center the vertex is on (for AA).
    unsafe fn draw_strip(&self, vertices: &[[f32; 3]], color: Color, alpha: f32) {
        let gl = &self.gl;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
        gl.viewport(0, 0, self.width as i32, self.height as i32);

        gl.enable(glow::BLEND);
        // Porter-Duff "over": proper alpha compositing into the FBO
        gl.blend_func_separate(
            glow::SRC_ALPHA,
            glow::ONE_MINUS_SRC_ALPHA,
            glow::ONE,
            glow::ONE_MINUS_SRC_ALPHA,
        );

        gl.use_program(Some(self.program));
        gl.uniform_4_f32(
            Some(&self.u_color),
            color.0 as f32,
            color.1 as f32,
            color.2 as f32,
            alpha,
        );

        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            cast_vert_slice(vertices),
            glow::STREAM_DRAW,
        );

        // stride = 12 bytes (3 floats × 4 bytes)
        gl.enable_vertex_attrib_array(self.a_pos);
        gl.vertex_attrib_pointer_f32(self.a_pos, 2, glow::FLOAT, false, 12, 0);

        gl.enable_vertex_attrib_array(self.a_cross);
        gl.vertex_attrib_pointer_f32(self.a_cross, 1, glow::FLOAT, false, 12, 8);

        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, vertices.len() as i32);

        gl.disable_vertex_attrib_array(self.a_pos);
        gl.disable_vertex_attrib_array(self.a_cross);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
    }

    /// Blit the FBO texture to the default framebuffer, compositing over bg.
    unsafe fn blit_to_screen(&self, bg: Color) {
        let gl = &self.gl;
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.viewport(0, 0, self.width as i32, self.height as i32);

        gl.disable(glow::BLEND);
        gl.use_program(Some(self.blit_program));
        gl.uniform_3_f32(Some(&self.u_bg), bg.0 as f32, bg.1 as f32, bg.2 as f32);

        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(self.fbo_texture));

        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.blit_vbo));
        let a_pos = gl.get_attrib_location(self.blit_program, "a_pos").unwrap();
        gl.enable_vertex_attrib_array(a_pos);
        gl.vertex_attrib_pointer_f32(a_pos, 2, glow::FLOAT, false, 8, 0);

        gl.draw_arrays(glow::TRIANGLES, 0, 6);

        gl.disable_vertex_attrib_array(a_pos);
    }

    /// Clear the FBO to fully transparent.
    unsafe fn clear(&self) {
        let gl = &self.gl;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
        gl.viewport(0, 0, self.width as i32, self.height as i32);
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
    }
}

/// Cast &[f32] → &[u8].
fn cast_f32_slice(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}

/// Cast &[[f32; 3]] → &[u8].
fn cast_vert_slice(data: &[[f32; 3]]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 12) }
}

// ---------------------------------------------------------------------------
// Per-output surface state
// ---------------------------------------------------------------------------

struct OutputSurface {
    layer: LayerSurface,
    width: u32,
    height: u32,
    configured: bool,
    egl_surface: khronos_egl::Surface,
    #[allow(dead_code)]
    wl_egl_surface: wayland_egl::WlEglSurface,
    renderer: Option<GlRenderer>,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    qh: QueueHandle<Self>,

    egl_display: khronos_egl::Display,
    egl_context: khronos_egl::Context,
    egl_config: khronos_egl::Config,

    outputs: Vec<(wl_output::WlOutput, Option<OutputSurface>)>,
    harmonograph: Harmonograph,
    fg_colors: Vec<Color>,
    bg_color: Color,
    current_color: Color,
    steps_per_tick: u32,
    /// Per-axis NDC scale factors to keep the pattern square.
    /// Computed from the first configured output's dimensions.
    scale_x: f64,
    scale_y: f64,
}

impl App {
    fn pick_new_color(&mut self) {
        let mut rng = rand::thread_rng();
        if self.fg_colors.len() <= 1 {
            self.current_color = self.fg_colors[0];
            return;
        }
        loop {
            let c = self.fg_colors[rng.gen_range(0..self.fg_colors.len())];
            if c != self.current_color {
                self.current_color = c;
                return;
            }
        }
    }

    fn restart(&mut self) {
        let egl = khronos_egl::Instance::new(khronos_egl::Static);
        for (_wl, osurface) in &mut self.outputs {
            if let Some(os) = osurface {
                if let Some(ref renderer) = os.renderer {
                    egl.make_current(
                        self.egl_display,
                        Some(os.egl_surface),
                        Some(os.egl_surface),
                        Some(self.egl_context),
                    )
                    .unwrap();
                    unsafe { renderer.clear() };
                }
            }
        }
        self.harmonograph.randomize();
        self.pick_new_color();
    }

    fn tick(&mut self) {
        let color = self.current_color;
        let steps = self.steps_per_tick;
        let egl = khronos_egl::Instance::new(khronos_egl::Static);

        // Reduce alpha of existing content each frame
        self.fade_all_outputs(&egl);

        // Accumulate all simulation steps into one continuous triangle strip
        // so there are no gaps at segment joints.
        let mut verts: Vec<[f32; 3]> = Vec::new();

        for _ in 0..steps {
            if !self.harmonograph.advance() {
                // Draw whatever we accumulated before restarting
                if !verts.is_empty() {
                    self.draw_on_all_outputs(&egl, &verts, color);
                }
                self.restart();
                self.present_all(&egl);
                return;
            }

            // Append this segment's vertices to the continuous strip.
            // The Python version used `scale = min(w, h) * 0.4` in pixel
            // coords for both axes, keeping the pattern square. In NDC [-1,1]
            // we need per-axis scaling: the shorter axis gets scale 0.4
            // (pendulum ±1 maps to ±0.4 of NDC) while the longer axis is
            // shrunk by the aspect ratio to preserve 1:1 proportions.
            // Line width 0.002 in NDC ≈ 1.9px at 960 logical height.
            self.harmonograph.append_catmull_rom_strip(
                self.scale_x,
                self.scale_y,
                0.002,
                16,
                &mut verts,
            );
        }

        if !verts.is_empty() {
            self.draw_on_all_outputs(&egl, &verts, color);
            self.present_all(&egl);
        }
    }

    fn fade_all_outputs(&mut self, egl: &khronos_egl::Instance<khronos_egl::Static>) {
        for (_wl, osurface) in &mut self.outputs {
            if let Some(os) = osurface {
                if !os.configured {
                    continue;
                }
                if let Some(ref renderer) = os.renderer {
                    egl.make_current(
                        self.egl_display,
                        Some(os.egl_surface),
                        Some(os.egl_surface),
                        Some(self.egl_context),
                    )
                    .unwrap();
                    unsafe {
                        // Reduce alpha by 0.005 per frame at ~30fps.
                        // Lines fade to transparent over ~7 seconds.
                        renderer.fade(0.005);
                    }
                }
            }
        }
    }

    fn draw_on_all_outputs(
        &mut self,
        egl: &khronos_egl::Instance<khronos_egl::Static>,
        verts: &[[f32; 3]],
        color: Color,
    ) {
        for (_wl, osurface) in &mut self.outputs {
            if let Some(os) = osurface {
                if !os.configured {
                    continue;
                }
                if let Some(ref renderer) = os.renderer {
                    egl.make_current(
                        self.egl_display,
                        Some(os.egl_surface),
                        Some(os.egl_surface),
                        Some(self.egl_context),
                    )
                    .unwrap();
                    unsafe {
                        renderer.draw_strip(verts, color, 0.85);
                    }
                }
            }
        }
    }

    fn present_all(&mut self, egl: &khronos_egl::Instance<khronos_egl::Static>) {
        let bg = self.bg_color;
        for (_wl, osurface) in &mut self.outputs {
            if let Some(os) = osurface {
                if !os.configured {
                    continue;
                }
                if let Some(ref renderer) = os.renderer {
                    egl.make_current(
                        self.egl_display,
                        Some(os.egl_surface),
                        Some(os.egl_surface),
                        Some(self.egl_context),
                    )
                    .unwrap();
                    unsafe {
                        renderer.blit_to_screen(bg);
                    }
                    egl.swap_buffers(self.egl_display, os.egl_surface).unwrap();
                    os.layer
                        .wl_surface()
                        .damage_buffer(0, 0, os.width as i32, os.height as i32);
                    os.layer.commit();
                }
            }
        }
    }

    fn create_surface_for_output(&mut self, wl_output: &wl_output::WlOutput) {
        let info = self.output_state.info(wl_output);
        let (width, height) = info
            .as_ref()
            .and_then(|i| i.logical_size)
            .map(|(w, h)| (w as u32, h as u32))
            .unwrap_or((1920, 1080));

        info!(
            "Creating layer surface for output: {:?} ({}x{})",
            info.as_ref().and_then(|i| i.name.as_deref()),
            width,
            height
        );

        let surface = self.compositor_state.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Background,
            Some("wl-harmonograph"),
            Some(wl_output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(width, height);
        layer.commit();

        // Create EGL surface
        let wl_surface = layer.wl_surface();
        let wl_egl_surface =
            wayland_egl::WlEglSurface::new(wl_surface.id(), width as i32, height as i32)
                .expect("create WlEglSurface");

        let egl = khronos_egl::Instance::new(khronos_egl::Static);
        let egl_surface = unsafe {
            egl.create_window_surface(
                self.egl_display,
                self.egl_config,
                wl_egl_surface.ptr() as khronos_egl::NativeWindowType,
                None,
            )
            .expect("create EGL surface")
        };

        // Disable vsync — we drive frame pacing with calloop timer
        egl.make_current(
            self.egl_display,
            Some(egl_surface),
            Some(egl_surface),
            Some(self.egl_context),
        )
        .unwrap();
        egl.swap_interval(self.egl_display, 0).unwrap();

        let os = OutputSurface {
            layer,
            width,
            height,
            configured: false,
            egl_surface,
            wl_egl_surface,
            renderer: None,
        };

        for (wl, slot) in &mut self.outputs {
            if wl == wl_output {
                *slot = Some(os);
                return;
            }
        }
        self.outputs.push((wl_output.clone(), Some(os)));
    }

    fn init_renderer_for_output(&mut self, idx: usize) {
        let egl = khronos_egl::Instance::new(khronos_egl::Static);
        if let Some((_wl, Some(os))) = self.outputs.get_mut(idx) {
            if os.renderer.is_some() {
                return;
            }
            egl.make_current(
                self.egl_display,
                Some(os.egl_surface),
                Some(os.egl_surface),
                Some(self.egl_context),
            )
            .unwrap();

            let gl = unsafe {
                glow::Context::from_loader_function(|name| {
                    let egl = khronos_egl::Instance::new(khronos_egl::Static);
                    egl.get_proc_address(name)
                        .map_or(std::ptr::null(), |p| p as *const _)
                })
            };

            let renderer = unsafe { GlRenderer::new(gl, os.width, os.height) };
            os.renderer = Some(renderer);
            info!("GL renderer initialized for {}x{}", os.width, os.height);
        }
    }

    /// Recompute NDC scale factors from the largest configured output.
    ///
    /// The Python version used `scale = min(w, h) * 0.4` in pixel coordinates
    /// for both axes, keeping the pattern square. In NDC [-1, 1], each axis
    /// spans its full pixel dimension, so we need:
    ///   scale_x = min(w, h) * 0.4 / (w / 2) = min(w, h) * 0.8 / w
    ///   scale_y = min(w, h) * 0.4 / (h / 2) = min(w, h) * 0.8 / h
    /// For the shorter axis this gives 0.8, for the longer it's smaller.
    fn update_scales(&mut self) {
        let mut max_w = 0u32;
        let mut max_h = 0u32;
        for (_wl, osurface) in &self.outputs {
            if let Some(os) = osurface {
                if os.configured {
                    max_w = max_w.max(os.width);
                    max_h = max_h.max(os.height);
                }
            }
        }
        if max_w > 0 && max_h > 0 {
            let min_dim = max_w.min(max_h) as f64;
            self.scale_x = min_dim * 0.8 / max_w as f64;
            self.scale_y = min_dim * 0.8 / max_h as f64;
            info!(
                "Updated scales: {:.3} x {:.3} (from {}x{})",
                self.scale_x, self.scale_y, max_w, max_h
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Wayland protocol handlers
// ---------------------------------------------------------------------------

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.outputs.push((output.clone(), None));
        self.create_surface_for_output(&output);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.outputs.retain(|(wl, _)| wl != &output);
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {}

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let mut found_idx = None;
        for (i, (_wl, osurface)) in self.outputs.iter_mut().enumerate() {
            if let Some(os) = osurface {
                if os.layer.wl_surface() == layer.wl_surface() {
                    let new_w = configure.new_size.0.max(1);
                    let new_h = configure.new_size.1.max(1);
                    os.width = new_w;
                    os.height = new_h;
                    os.wl_egl_surface.resize(new_w as i32, new_h as i32, 0, 0);
                    os.configured = true;
                    found_idx = Some(i);
                    info!("Layer surface configured: {}x{}", new_w, new_h);
                    break;
                }
            }
        }
        if let Some(idx) = found_idx {
            self.init_renderer_for_output(idx);
            self.update_scales();
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

// ---------------------------------------------------------------------------
// EGL initialization
// ---------------------------------------------------------------------------

fn init_egl(
    conn: &Connection,
) -> (
    khronos_egl::Display,
    khronos_egl::Context,
    khronos_egl::Config,
) {
    let egl = khronos_egl::Instance::new(khronos_egl::Static);

    let wl_display = conn.backend().display_ptr() as *mut std::ffi::c_void;

    // Get EGL display from Wayland display
    let egl_display = unsafe {
        egl.get_display(wl_display as khronos_egl::NativeDisplayType)
            .expect("get EGL display")
    };
    egl.initialize(egl_display).expect("EGL initialize");

    let attributes = [
        khronos_egl::RED_SIZE,
        8,
        khronos_egl::GREEN_SIZE,
        8,
        khronos_egl::BLUE_SIZE,
        8,
        khronos_egl::ALPHA_SIZE,
        8,
        khronos_egl::SURFACE_TYPE,
        khronos_egl::WINDOW_BIT,
        khronos_egl::RENDERABLE_TYPE,
        khronos_egl::OPENGL_ES2_BIT,
        khronos_egl::NONE,
    ];

    let config = egl
        .choose_first_config(egl_display, &attributes)
        .expect("choose EGL config")
        .expect("no matching EGL config");

    let context_attrs = [
        khronos_egl::CONTEXT_MAJOR_VERSION,
        2,
        khronos_egl::CONTEXT_MINOR_VERSION,
        0,
        khronos_egl::NONE,
    ];

    let context = egl
        .create_context(egl_display, config, None, &context_attrs)
        .expect("create EGL context");

    (egl_display, context, config)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    env_logger::init();

    let conn = Connection::connect_to_env().expect("Failed to connect to Wayland");
    let (globals, mut event_queue) = registry_queue_init(&conn).expect("registry init");
    let qh = event_queue.handle();

    let compositor_state =
        CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell not available");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm not available");
    let output_state = OutputState::new(&globals, &qh);
    let registry_state = RegistryState::new(&globals);

    let (egl_display, egl_context, egl_config) = init_egl(&conn);

    let (fg_colors, bg_color) = colors_from_env();
    let mut rng = rand::thread_rng();
    let current_color = fg_colors[rng.gen_range(0..fg_colors.len())];

    let mut app = App {
        registry_state,
        output_state,
        compositor_state,
        layer_shell,
        shm,
        qh: qh.clone(),
        egl_display,
        egl_context,
        egl_config,
        outputs: Vec::new(),
        harmonograph: Harmonograph::new(),
        fg_colors,
        bg_color,
        current_color,
        steps_per_tick: 1,
        scale_x: 0.4,
        scale_y: 0.4,
    };

    event_queue.roundtrip(&mut app).expect("roundtrip");

    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("calloop event loop");
    let loop_handle = event_loop.handle();

    WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle.clone())
        .expect("insert wayland source");

    // ~30fps × 1 step/tick = 30 steps/sec (matches original Python pacing)
    loop_handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(33)),
            |_, _, app| {
                app.tick();
                TimeoutAction::ToDuration(Duration::from_millis(33))
            },
        )
        .expect("insert timer");

    info!("Starting event loop (GPU-accelerated)");
    loop {
        event_loop.dispatch(None, &mut app).expect("dispatch");
    }
}
