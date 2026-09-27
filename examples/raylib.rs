use {
    arna::{
        Arna, TEMP_ARENAS, TempArenas, aformat,
        dynlib::DynamicLibrary,
        imui::{
            Align, BLUE, BackendBase, BackendFromBase, Ctx,
            Direction::Top2Bottom, DrawCmdData, FontId, GREEN, Glyph,
            InputState, Layout::Flex, RED, RectCmdData, TextCmdData, WHITE,
        },
    },
    core::{
        array,
        ffi::c_char,
        mem::{MaybeUninit, transmute},
    },
    std::{
        ffi::CString, path::PathBuf, ptr::NonNull, sync::atomic::AtomicPtr,
        time::SystemTime,
    },
};

pub struct App {
    font: FontId,
    ctx: Ctx,
    backend: RaylibBackend,
}

type RunFn = unsafe extern "C" fn(&mut App);

#[derive(Default)]
pub struct Module {
    last_mod: Option<SystemTime>,
    changing_files: Vec<PathBuf>,
    libs: Vec<DynamicLibrary>,
    lib_paths: Vec<PathBuf>,
    run: *mut RunFn,
}

impl Drop for Module {
    fn drop(&mut self) {
        self.libs.clear();
        for path in &self.lib_paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Module {
    pub fn reload(&mut self) {
        let should_reload;

        if self.changing_files.len() == 0 {
            should_reload = true;
        } else {
            let mut last_mod = SystemTime::UNIX_EPOCH;
            for path in self.changing_files.iter() {
                let mod_time = path
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                last_mod = last_mod.max(mod_time);
            }

            should_reload =
                self.last_mod.unwrap_or(SystemTime::UNIX_EPOCH) < last_mod;
            self.last_mod = Some(last_mod);
        };

        let dir = if cfg!(debug_assertions) { "debug" } else { "release" };

        if should_reload {
            let status = std::process::Command::new("cargo")
                .args([
                    "build",
                    "--example",
                    "raylib-lib",
                    "--features",
                    "example-link-raylib,dll",
                ])
                .args(cfg!(not(debug_assertions)).then(|| "--release"))
                .status()
                .unwrap();

            if !status.success() {
                return;
            }

            let built_lib = PathBuf::from(format!("target/{dir}/examples"))
                .join(format!(
                    "{}raylib_lib{}",
                    std::env::consts::DLL_PREFIX,
                    std::env::consts::DLL_SUFFIX,
                ));
            let hot_lib = built_lib.with_file_name(format!(
                "{}raylib_lib.hot-{}{}",
                std::env::consts::DLL_PREFIX,
                self.libs.len(),
                std::env::consts::DLL_SUFFIX,
            ));
            std::fs::copy(&built_lib, &hot_lib).unwrap();

            let lib = DynamicLibrary::open(Some(&hot_lib)).unwrap();

            let tmp_arenas_override = unsafe {
                lib.symbol::<AtomicPtr<TempArenas>>("ARNA_TEMP_ARENAS_OVERRIDE")
                    .unwrap()
            };
            unsafe {
                tmp_arenas_override.as_ref().unwrap().store(
                    &TEMP_ARENAS as *const _ as *mut _,
                    core::sync::atomic::Ordering::Relaxed,
                );
            };

            self.run = unsafe { lib.symbol("run") }.unwrap();

            self.libs.push(lib);
            self.lib_paths.push(hot_lib);

            self.changing_files.clear();

            let used_files = std::fs::read_to_string(format!(
                "target/{dir}/examples/libraylib_lib.d"
            ))
            .unwrap();

            self.changing_files.extend(
                used_files
                    .split_whitespace()
                    .skip(1)
                    .filter(|v| !v.is_empty())
                    .map(|s| PathBuf::from(s)),
            );
            self.last_mod = self
                .changing_files
                .iter()
                .filter_map(|path| {
                    path.metadata().and_then(|m| m.modified()).ok()
                })
                .max();
        }
    }

    pub unsafe fn run(&mut self, state: &mut App) {
        unsafe { transmute::<_, RunFn>(self.run)(state) }
    }
}

#[allow(improper_ctypes)]
unsafe extern "C" {
    #[link_name = "SetConfigFlags"]
    pub fn set_config_flags(flags: u32);
    #[link_name = "SetTargetFPS"]
    pub fn set_target_fps(flags: i32);
    #[link_name = "InitWindow"]
    fn init_window(width: i32, height: i32, name: *const c_char);
    #[link_name = "WindowShouldClose"]
    fn window_should_close() -> bool;
    #[link_name = "BeginDrawing"]
    fn begin_drawing();
    #[link_name = "ClearBackground"]
    fn clear_background(color: RaylibColor);
    #[link_name = "EndDrawing"]
    fn end_drawing();
    #[link_name = "CloseWindow"]
    fn close_window();
    #[link_name = "DrawRectangleRec"]
    fn draw_rectangle(rec: RaylibRectangle, color: RaylibColor);
    #[link_name = "GetMousePosition"]
    fn get_mouse_position() -> [f32; 2];
    #[link_name = "IsMouseButtonPressed"]
    fn is_mouse_button_pressed(button: MouseButton) -> bool;
    #[link_name = "GetScreenWidth"]
    pub fn get_screen_width() -> i32;
    #[link_name = "GetScreenHeight"]
    pub fn get_screen_height() -> i32;
    #[link_name = "GetGlyphIndex"]
    pub fn get_glyph_index(font: Font, ch: char) -> i32;
    #[link_name = "GetFontDefault"]
    pub fn get_font_default() -> Font;
    #[link_name = "DrawTextEx"]
    fn draw_text_ex(
        font: Font,
        text: *const i8,
        position: [f32; 2],
        fontSize: f32,
        spacing: f32,
        tint: RaylibColor,
    );
}

#[repr(C)]
struct RaylibColor {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

#[repr(C)]
pub struct RaylibRectangle {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left = 0,
    Right = 1,
    Middle = 2,
    Side = 3,
    Extra = 4,
    Forward = 5,
    Back = 6,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RaylibGlyphInfo {
    pub value: char,
    pub offset_x: i32,
    pub offset_y: i32,
    pub advance_x: i32,
    pub image: RaylibImage,
}

pub type RaylibPixelFormat = i32;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RaylibImage {
    pub data: *mut (),
    pub width: i32,
    pub height: i32,
    pub mipmaps: i32,
    pub format: RaylibPixelFormat,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Font {
    pub base_size: i32,
    pub glyph_count: i32,
    pub glyph_padding: i32,
    pub texture: Texture,
    pub recs: *mut RaylibRectangle,
    pub glyphs: *mut RaylibGlyphInfo,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Texture {
    pub id: u32,
    pub width: i32,
    pub height: i32,
    pub mipmaps: i32,
    pub format: RaylibPixelFormat,
}

pub const FLAG_WINDOW_RESIZABLE: u32 = 0x0000_0004;

struct RaylibBackend;

impl BackendBase for RaylibBackend {
    fn get_font_base_size(&mut self, font: arna::imui::FontId) -> f32 {
        let font = font.0.as_ptr().cast::<Font>();
        unsafe { (*font).base_size as f32 }
    }

    fn get_glypy_data(
        &mut self,
        font: arna::imui::FontId,
        ch: char,
    ) -> Option<arna::imui::Glyph> {
        let font = unsafe { font.0.as_ptr().cast::<Font>().read() };
        let idx = unsafe { get_glyph_index(font, ch) };
        if idx < 0 || idx >= font.glyph_count {
            None
        } else {
            let data = unsafe { font.glyphs.add(idx as usize).read() };
            let rec = unsafe { font.recs.add(idx as usize).read() };
            Some(Glyph {
                offset_x: data.offset_x as f32,
                advance_x: data.advance_x as f32,
                width: rec.width,
            })
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn run(app: &mut App) {
    let ctx = &mut app.ctx;

    {
        let _ss = ctx.push_style(|s| {
            s.margin([4.])
                .padding([4.])
                .font(app.font)
                .font_size(10.)
                .font_spacing(1.)
                .fg_color(BLUE)
        });

        let _el = ctx.el(0).style(|i, s| {
            s.width_px(unsafe { get_screen_width() } as usize)
                .height_px(unsafe { get_screen_height() } as usize)
                .fg_color(if i.hovered() { RED } else { WHITE })
                .layout(Flex)
                //.align_y(Align::Center)
                .direction(Top2Bottom)
                .text(include_str!("raylib.rs"))
        });

        if false {
            let _wrap = ctx.el(0).style(|_, s| {
                s.width_perc(1.)
                    .bg_color(BLUE)
                    .align_x(Align::Center)
                    .gap_px(10)
                    .gap_px(0)
            });

            for _ in 0..300 {
                ctx.el(0).style(|_, s| s.width_px(1).bg_color(RED));
            }

            for _ in 0..3 {
                let _foo =
                    ctx.el(0).style(|_, s| s.width_px(4 * 30).bg_color(GREEN));

                for _ in 0..30 {
                    ctx.el(0).style(|_, s| s.width_px(1).bg_color(RED));
                }

                ctx.el(0).style(|_, s| s.width_grow().bg_color(RED));
            }

            {
                let _ss = ctx.el("proba").style(|i, s| {
                    s.gap_px(4).bg_color(if i.any_hovered() {
                        RED
                    } else {
                        GREEN
                    })
                });

                for i in 0..16 {
                    ctx.el("brahma").idx(i).style(|i, s| {
                        s.margin([0.]).bg_color(if i.hovered() {
                            WHITE
                        } else {
                            BLUE
                        })
                    });
                }
            }

            ctx.el(0)
                .style(|_, s| s.text("Lorem ipsum and so on").bg_color(WHITE));

            {
                let _lup =
                    ctx.el(0).style(|_, s| s.bg_color(GREEN).width_px(100));

                ctx.el(0).style(|_, s| {
                    s.text("there is enought text to overflow")
                        .bg_color(WHITE)
                        .width_grow()
                });
            }
        }

        for i in 0..0 {
            let _wrap = ctx.el(0).style(|_, s| {
                s.width_perc(1.)
                    .bg_color(BLUE)
                    .align_x(Align::Center)
                    .direction(Top2Bottom)
                    .gap_px(10)
            });

            let ss =
                ctx.push_style(|s| s.width_px(40).height_fit().bg_color(GREEN));

            for j in i * 2..i * 2 + 2 {
                if ctx
                    .el("btn")
                    .idx(j)
                    .style(|b, s| {
                        s.text(if b.hovered() {
                            aformat!(b, "no {j}")
                        } else {
                            "yes".into()
                        })
                        .bg_color(if b.hovered() { RED } else { WHITE })
                        .min_width_px(30)
                        .width_perc(1.)
                        .align_x(Align::End)
                    })
                    .hovered()
                    && unsafe { is_mouse_button_pressed(MouseButton::Left) }
                {
                    println!("yayayay {j}")
                }

                if ctx
                    .el("btn-2")
                    .idx(j)
                    .style(|b, s| {
                        s.text(if b.hovered() {
                            aformat!(b, "no {i}")
                        } else {
                            "yes".into()
                        })
                        .self_align(Align::Center)
                        .align_x(Align::Center)
                    })
                    .hovered()
                    && unsafe { is_mouse_button_pressed(MouseButton::Left) }
                {
                    println!("nayyaya {j}")
                }
            }

            drop(ss);
        }
    }

    let cmds = Arna::scratch(0);

    for cmd in app.ctx.cmds(
        BackendFromBase::new(&mut app.backend),
        &cmds,
        InputState { mouse_pos: unsafe { get_mouse_position() } },
    ) {
        match cmd.data {
            DrawCmdData::Text(TextCmdData {
                x,
                y,
                size,
                spacing,
                content,
                font,
                color,
            }) => unsafe {
                draw_text_ex(
                    font.0.as_ptr().cast::<Font>().read(),
                    CString::new(&*app.ctx.text(content)).unwrap().as_ptr(),
                    [x, y],
                    size,
                    spacing,
                    RaylibColor {
                        r: (color >> 24) as u8,
                        g: (color >> 16) as u8,
                        b: (color >> 8) as u8,
                        a: (color >> 0) as u8,
                    },
                );
            },
            DrawCmdData::Rect(RectCmdData { x, y, width, height, color }) => unsafe {
                draw_rectangle(
                    RaylibRectangle { x, y, width, height },
                    RaylibColor {
                        r: (color >> 24) as u8,
                        g: (color >> 16) as u8,
                        b: (color >> 8) as u8,
                        a: (color >> 0) as u8,
                    },
                );
            },
            DrawCmdData::Null => {}
        }
    }
}

pub fn main() {
    unsafe { set_config_flags(FLAG_WINDOW_RESIZABLE) };
    unsafe { init_window(800, 600, c"Arna IMUI example".as_ptr()) };
    unsafe { set_target_fps(60) };

    crate::Arna::init_temp_arenas(array::from_fn(|_| {
        crate::Arna::from(
            vec![MaybeUninit::<u8>::uninit(); 1024 * 1024].into_boxed_slice(),
        )
    }));

    let font = unsafe { get_font_default() };

    let mut app = App {
        ctx: Ctx::default(),
        font: FontId(NonNull::from_ref(&font).cast()),
        backend: RaylibBackend,
    };

    let mut module = Module::default();

    while !unsafe { window_should_close() } {
        unsafe { begin_drawing() };
        unsafe {
            clear_background(RaylibColor { r: 30, g: 35, b: 45, a: 255 })
        };

        module.reload();
        unsafe { module.run(&mut app) };

        unsafe { end_drawing() };
    }

    unsafe { close_window() };
}
