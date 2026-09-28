use {
    arna::{
        Arna, aformat,
        dynlib::hot::Module,
        id,
        imui::{
            Align, BLUE, BackendBase, Color, Ctx, Direction::Top2Bottom,
            DrawCmdData, FontId, GREEN, Glyph, InputState, Layout::Flex, RED,
            RectCmdData, TextCmdData, WHITE, lerp, lerp_color,
        },
    },
    core::{
        array,
        ffi::{c_char, c_void},
        mem::MaybeUninit,
    },
    std::ptr::NonNull,
};

pub struct App {
    font: FontId,
    _font_data: Box<IndexedFont>,
    ctx: Ctx,
    backend: RaylibBackend,
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
    #[link_name = "GetFontDefault"]
    pub fn get_font_default() -> Font;
    #[link_name = "DrawTexturePro"]
    pub fn draw_texture_pro(
        texture: RaylibTexture,
        source: RaylibRectangle,
        dest: RaylibRectangle,
        origin: [f32; 2],
        rotation: f32,
        tint: RaylibColor,
    );
}

pub struct IndexedFont {
    font: Font,
    index: Vec<u8>,
}

// TODO: this only works because the char count in default font is % 16
pub fn char_hash(char: char) -> u8 {
    (arna::imui::mix_u32(char as u32, 0) as u8).max(1)
}

pub fn get_glyph_index(font: &IndexedFont, char: char) -> Option<usize> {
    arna::SimdIter::new(&font.index, char_hash(char))
        .find(|&idx| unsafe { (*font.font.glyphs.add(idx)).value == char })
}

impl App {
    fn new() -> Self {
        let mut font = Box::new(IndexedFont {
            font: unsafe { get_font_default() },
            index: vec![],
        });

        for i in 0..font.font.glyph_count as usize {
            font.index
                .push(char_hash(unsafe { (*font.font.glyphs.add(i)).value }));
        }
        font.index.resize(arna::simd::align_forward(font.index.len()), 0);

        Self {
            ctx: Ctx::default(),
            font: FontId(NonNull::from_ref(&*font).cast()),
            _font_data: font,
            backend: RaylibBackend,
        }
    }
}

fn init_temp_arenas() {
    Arna::init_temp_arenas(array::from_fn(|_| {
        Arna::from(
            vec![MaybeUninit::<u8>::uninit(); 1024 * 1024].into_boxed_slice(),
        )
    }));
}

pub fn draw_text_ex(
    font: &IndexedFont,
    text: &str,
    position: [f32; 2],
    font_size: f32,
    spacing: f32,
    line_spacing: f32,
    tint: RaylibColor,
) {
    let mut text_offset_y = 0.0;
    let mut text_offset_x = 0.0;

    let scale_factor = font_size / font.font.base_size as f32;

    for codepoint in text.chars() {
        let Some(index) = get_glyph_index(font, codepoint) else {
            continue;
        };

        if codepoint == '\n' {
            text_offset_y += font_size + line_spacing;
            text_offset_x = 0.0;
        } else {
            if codepoint != ' ' && codepoint != '\t' {
                unsafe {
                    draw_text_codepoint(
                        font.font,
                        index,
                        [
                            position[0] + text_offset_x,
                            position[1] + text_offset_y,
                        ],
                        font_size,
                        tint,
                    );
                }
            }

            let glyph = unsafe { font.font.glyphs.add(index).read() };
            let rec = unsafe { font.font.recs.add(index).read() };

            if glyph.advance_x == 0 {
                text_offset_x += rec.width * scale_factor + spacing;
            } else {
                text_offset_x +=
                    glyph.advance_x as f32 * scale_factor + spacing;
            }
        }
    }
}

pub unsafe fn draw_text_codepoint(
    font: Font,
    codepoint_index: usize,
    position: [f32; 2],
    font_size: f32,
    tint: RaylibColor,
) {
    let index = codepoint_index;
    let scale_factor = font_size / font.base_size as f32;

    let glyph = unsafe { font.glyphs.add(index).read() };
    let rec = unsafe { font.recs.add(index).read() };
    let padding = font.glyph_padding as f32;

    let dst_rec = RaylibRectangle {
        x: position[0] + glyph.offset_x as f32 * scale_factor
            - padding * scale_factor,
        y: position[1] + glyph.offset_y as f32 * scale_factor
            - padding * scale_factor,
        width: (rec.width + 2.0 * padding) * scale_factor,
        height: (rec.height + 2.0 * padding) * scale_factor,
    };

    let src_rec = RaylibRectangle {
        x: rec.x - padding,
        y: rec.y - padding,
        width: rec.width + 2.0 * padding,
        height: rec.height + 2.0 * padding,
    };

    unsafe {
        draw_texture_pro(font.texture, src_rec, dst_rec, [0.0, 0.0], 0.0, tint);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RaylibColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
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
    pub texture: RaylibTexture,
    pub recs: *mut RaylibRectangle,
    pub glyphs: *mut RaylibGlyphInfo,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RaylibTexture {
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
        let font = unsafe { &*font.0.as_ptr().cast::<IndexedFont>() };
        font.font.base_size as f32
    }

    fn get_glypy_data(
        &mut self,
        font: arna::imui::FontId,
        ch: char,
    ) -> Option<arna::imui::Glyph> {
        let font = unsafe { &*font.0.as_ptr().cast::<IndexedFont>() };
        let idx = get_glyph_index(font, ch)?;
        let data = unsafe { font.font.glyphs.add(idx as usize).read() };
        let rec = unsafe { font.font.recs.add(idx as usize).read() };
        Some(Glyph {
            offset_x: data.offset_x as f32,
            advance_x: data.advance_x as f32,
            width: rec.width,
        })
    }
}

#[unsafe(no_mangle)]
extern "C" fn create() -> *mut c_void {
    Box::into_raw(Box::new(App::new())).cast()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn destroy(state: *mut c_void) {
    drop(unsafe { Box::from_raw(state.cast::<App>()) });
}

#[unsafe(no_mangle)]
extern "C" fn state_version() -> usize {
    1
}

#[unsafe(no_mangle)]
unsafe extern "C" fn run(state: *mut c_void) {
    render(unsafe { &mut *state.cast::<App>() });
}

fn render(app: &mut App) {
    let ctx = &mut app.ctx;

    {
        let _ss = ctx.push_style(|s| {
            s.margin([4.])
                .padding([4.])
                .font(app.font)
                .font_size(10.)
                .font_spacing(1.)
                .fg_color(BLUE)
                .font_line_spacing(2.)
        });

        let _el = ctx.anon().style(|s| {
            s.width(unsafe { get_screen_width() } as f32)
                .height(unsafe { get_screen_height() } as f32)
                .layout(Flex)
                .align_y(Align::Center)
                .direction(Top2Bottom)
            //.text(include_str!("raylib.rs"))
        });

        if true {
            let _wrap = ctx.anon().style(|s| {
                s.width_perc(1.)
                    .bg_color(BLUE)
                    .align_x(Align::Center)
                    .gap(10.)
                    .gap(0.)
            });

            for _ in 0..300 {
                ctx.anon().style(|s| s.width(1.).bg_color(RED));
            }

            for _ in 0..3 {
                let _foo =
                    ctx.anon().style(|s| s.width(4. * 30.).bg_color(GREEN));

                for _ in 0..30 {
                    ctx.anon().style(|s| s.width(1.).bg_color(RED));
                }

                ctx.anon().style(|s| s.width_grow().bg_color(RED));
            }

            {
                let _ss = ctx.el(id!("proba")).style(|i, s| {
                    s.gap(4.).bg_color(if i.any_hovered() {
                        RED
                    } else {
                        GREEN
                    })
                });

                for i in 0..16 {
                    ctx.el(id!("brahma").idx(i)).style(|i, s| {
                        s.margin([0.]).bg_color(if i.hovered() {
                            WHITE
                        } else {
                            BLUE
                        })
                    });
                }
            }

            ctx.anon()
                .style(|s| s.text("Lorem ipsum and so on").bg_color(WHITE));

            {
                let _lup = ctx.anon().style(|s| s.bg_color(GREEN).width(100.));

                ctx.anon().style(|s| {
                    s.text("there is enought text to overflow")
                        .bg_color(WHITE)
                        .width_grow()
                });
            }
        }

        for i in 0..1 {
            let _wrap = ctx.anon().style(|s| {
                s.width_perc(1.)
                    .bg_color(BLUE)
                    .align_x(Align::Center)
                    .direction(Top2Bottom)
                    .gap(10.)
            });

            let ss =
                ctx.push_style(|s| s.width(40.).height_fit().bg_color(GREEN));

            for j in i * 2..i * 2 + 2 {
                if ctx
                    .el(id!("btn", j))
                    .style(|b, s| {
                        s.text(if b.hovered() {
                            aformat!(ctx, "no {j}")
                        } else {
                            "yes".into()
                        })
                        .bg_color(if b.hovered() { RED } else { WHITE })
                        .min_width(30.)
                        .width_perc(1.)
                        .align_x(Align::End)
                    })
                    .hovered()
                    && unsafe { is_mouse_button_pressed(MouseButton::Left) }
                {
                    println!("yayayay {j} {:?}", ctx.elem_by_id(id!("btn", j)))
                }

                let _foo = ctx.anon().style(|s| {
                    s.width(50.)
                        .height(50.)
                        .padding([0.])
                        .align_x(Align::Center)
                        .self_align(Align::Center)
                });

                if ctx
                    .el(id!("btn-2", j))
                    .style(|b, s| {
                        let p = b.prev();
                        let target = if b.hovered() { RED } else { GREEN };
                        let width_target = if b.hovered() { 100. } else { 50. };
                        let height_target =
                            if !b.hovered() { 50. } else { 100. };
                        s.text(if b.hovered() {
                            aformat!(ctx, "no {i}")
                        } else {
                            "yes".into()
                        })
                        .margin([0.])
                        .bg_color(lerp_color(p.style.bg_color, target, 0.2))
                        .width(lerp(p.size.x, width_target, 0.3))
                        .height(lerp(p.size.y, height_target, 0.3))
                        .self_align(Align::Center)
                        .align_x(Align::Center)
                        .layer(1)
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
        &mut app.backend,
        &cmds,
        InputState { mouse_pos: unsafe { get_mouse_position() } },
    ) {
        match cmd.data {
            DrawCmdData::Text(TextCmdData {
                x,
                y,
                size,
                spacing,
                line_spacing,
                content,
                font,
                color,
            }) => unsafe {
                draw_text_ex(
                    &*font.0.as_ptr().cast::<IndexedFont>(),
                    &*app.ctx.text(content),
                    [x, y],
                    size,
                    spacing,
                    line_spacing,
                    conv_color(color),
                );
            },
            DrawCmdData::Rect(RectCmdData { x, y, width, height, color }) => unsafe {
                draw_rectangle(
                    RaylibRectangle { x, y, width, height },
                    conv_color(color),
                );
            },
        }
    }

    fn conv_color(color: Color) -> RaylibColor {
        RaylibColor {
            r: (color >> 24) as u8,
            g: (color >> 16) as u8,
            b: (color >> 8) as u8,
            a: (color >> 0) as u8,
        }
    }
}

pub fn main() {
    unsafe { set_config_flags(FLAG_WINDOW_RESIZABLE) };
    unsafe { init_window(800, 600, c"Arna IMUI example".as_ptr()) };
    unsafe { set_target_fps(60) };

    init_temp_arenas();

    let mut app = App::new();
    let mut module = Module::new(
        "create",
        "destroy",
        "state_version",
        [
            "build",
            "--example",
            "raylib-lib",
            "--features",
            "example-link-raylib,dll",
        ],
    );

    while !unsafe { window_should_close() } {
        unsafe { begin_drawing() };
        unsafe {
            clear_background(RaylibColor { r: 30, g: 35, b: 45, a: 255 })
        };

        if cfg!(debug_assertions) {
            unsafe { module.reload_if_changed() };
            unsafe { module.call("run").unwrap() };
        } else {
            render(&mut app);
        }

        unsafe { end_drawing() };
    }

    unsafe { close_window() };
}
