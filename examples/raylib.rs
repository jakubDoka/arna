use {
    arna::{
        Arna, aformat,
        dynlib::hot::Module,
        hot_api, id,
        imui::{
            Align, BLUE, Backend, BackendBase, BackendFromBase, Color, Ctx,
            Dim,
            Direction::{Left2Right, Top2Bottom},
            DrawCmdData, ElemBuilder, ElemID, FontId, GREEN, Glyph, InputState,
            Layout::Flex,
            RED, RectCmdData, StartClipData, Style, TextCmdData, lerp,
            lerp_color,
        },
    },
    core::{
        array,
        ffi::{c_char, c_void},
        mem::MaybeUninit,
    },
    std::ptr::NonNull,
};

pub const WHITE: Color = 0xccccccff;

pub struct App {
    font: FontId,
    _font_data: Box<IndexedFont>,
    ctx: Ctx,
    backend: RaylibBackend,
    selection: [[f32; 2]; 2],
    focused: ElemID,
}

#[allow(improper_ctypes)]
unsafe extern "C" {
    #[link_name = "SetConfigFlags"]
    pub fn set_config_flags(flags: u32);
    #[link_name = "SetTargetFPS"]
    pub fn set_target_fps(flags: i32);
    #[link_name = "InitWindow"]
    pub fn init_window(width: i32, height: i32, name: *const c_char);
    #[link_name = "WindowShouldClose"]
    pub fn window_should_close() -> bool;
    #[link_name = "BeginDrawing"]
    pub fn begin_drawing();
    #[link_name = "ClearBackground"]
    pub fn clear_background(color: RaylibColor);
    #[link_name = "EndDrawing"]
    pub fn end_drawing();
    #[link_name = "CloseWindow"]
    pub fn close_window();
    #[link_name = "DrawRectangleRec"]
    pub fn draw_rectangle(rec: RaylibRectangle, color: RaylibColor);
    #[link_name = "GetMousePosition"]
    pub fn get_mouse_position() -> [f32; 2];
    #[link_name = "IsMouseButtonPressed"]
    pub fn is_mouse_button_pressed(button: MouseButton) -> bool;
    #[link_name = "IsMouseButtonDown"]
    pub fn is_mouse_button_down(button: MouseButton) -> bool;
    #[link_name = "IsMouseButtonReleased"]
    pub fn is_mouse_button_released(button: MouseButton) -> bool;
    #[link_name = "IsKeyPressed"]
    pub fn is_key_pressed(button: RaylibKey) -> bool;
    #[link_name = "IsKeyPressedRepeat"]
    pub fn is_key_pressed_repeat(button: RaylibKey) -> bool;
    #[link_name = "IsKeyDown"]
    pub fn is_key_down(button: RaylibKey) -> bool;
    #[link_name = "IsKeyReleased"]
    pub fn is_key_released(button: RaylibKey) -> bool;
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
    #[link_name = "GetMouseWheelMoveV"]
    pub fn get_mouse_wheel_move() -> [f32; 2];
    #[link_name = "BeginScissorMode"]
    pub fn begin_scissor_mode(x: i32, y: i32, width: i32, height: i32);
    #[link_name = "EndScissorMode"]
    pub fn end_scissor_mode();
    #[link_name = "GetCharPressed"]
    pub fn get_char_pressed() -> i32;
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RaylibKey {
    Null = 0,

    // Alphanumeric
    Apostrophe = 39,
    Comma = 44,
    Minus = 45,
    Period = 46,
    Slash = 47,
    Zero = 48,
    One = 49,
    Two = 50,
    Three = 51,
    Four = 52,
    Five = 53,
    Six = 54,
    Seven = 55,
    Eight = 56,
    Nine = 57,
    Semicolon = 59,
    Equal = 61,

    A = 65,
    B = 66,
    C = 67,
    D = 68,
    E = 69,
    F = 70,
    G = 71,
    H = 72,
    I = 73,
    J = 74,
    K = 75,
    L = 76,
    M = 77,
    N = 78,
    O = 79,
    P = 80,
    Q = 81,
    R = 82,
    S = 83,
    T = 84,
    U = 85,
    V = 86,
    W = 87,
    X = 88,
    Y = 89,
    Z = 90,

    LeftBracket = 91,
    Backslash = 92,
    RightBracket = 93,
    Grave = 96,

    // Function/navigation
    Space = 32,
    Escape = 256,
    Enter = 257,
    Tab = 258,
    Backspace = 259,
    Insert = 260,
    Delete = 261,
    Right = 262,
    Left = 263,
    Down = 264,
    Up = 265,
    PageUp = 266,
    PageDown = 267,
    Home = 268,
    End = 269,

    CapsLock = 280,
    ScrollLock = 281,
    NumLock = 282,
    PrintScreen = 283,
    Pause = 284,

    F1 = 290,
    F2 = 291,
    F3 = 292,
    F4 = 293,
    F5 = 294,
    F6 = 295,
    F7 = 296,
    F8 = 297,
    F9 = 298,
    F10 = 299,
    F11 = 300,
    F12 = 301,

    LeftShift = 340,
    LeftControl = 341,
    LeftAlt = 342,
    LeftSuper = 343,
    RightShift = 344,
    RightControl = 345,
    RightAlt = 346,
    RightSuper = 347,
    KeyboardMenu = 348,

    // Keypad
    Kp0 = 320,
    Kp1 = 321,
    Kp2 = 322,
    Kp3 = 323,
    Kp4 = 324,
    Kp5 = 325,
    Kp6 = 326,
    Kp7 = 327,
    Kp8 = 328,
    Kp9 = 329,
    KpDecimal = 330,
    KpDivide = 331,
    KpMultiply = 332,
    KpSubtract = 333,
    KpAdd = 334,
    KpEnter = 335,
    KpEqual = 336,

    // Android
    Back = 4,
    Menu = 5,
    VolumeUp = 24,
    VolumeDown = 25,
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
            selection: Default::default(),
            focused: Default::default(),
        }
    }
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

hot_api! {
    impl Hot for App {
        fn destroy() -> Self;

        fn create() -> Self {
            App::new()
        }

        fn state_version() -> usize {
            2 + core::mem::size_of::<App>()
        }

        fn run(slf: &mut Self) {
            render(slf);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct UpdateTextState {
    selection_start: usize,
    selection_end: usize,
    original_fg_color: Color,
    placeholder_is_active: bool,
}

fn update_focus(b: &ElemBuilder, focused: &mut ElemID) {
    if b.hovered() && unsafe { is_mouse_button_pressed(MouseButton::Left) } {
        *focused = b.prev().id;
    }
}

fn update_text(
    b: &ElemBuilder,
    style: &mut Style,
    focused: ElemID,
    placeholder: &'static str,
) {
    let p = b.prev();

    style.dont_wrap_text = true;
    style.clip_overflow = true;

    let mut state = b.update_state(|v: Option<UpdateTextState>| {
        v.unwrap_or(UpdateTextState {
            selection_start: 0,
            selection_end: 0,
            original_fg_color: 0,
            placeholder_is_active: true,
        })
    });

    if focused != p.id && state.placeholder_is_active {
        state.original_fg_color = style.fg_color;
    }

    style.scroll.x = p.style.scroll.x;

    let scratch = Arna::scratch(0);

    let mut text_edit = Vec::<char, _>::new_in(&scratch);
    if !state.placeholder_is_active {
        text_edit
            .extend(b.ctx.text(p.style.text).chars().filter(|&c| c != '\0'));
    }

    state.selection_end = usize::clamp(state.selection_end, 0, text_edit.len());
    state.selection_start =
        usize::clamp(state.selection_start, 0, text_edit.len());

    if focused == p.id {
        state.placeholder_is_active = false;

        while let ch @ 1.. = unsafe { get_char_pressed() } {
            if let Some(ch) = char::from_u32(ch as u32) {
                text_edit.insert(state.selection_end, ch);
                state.selection_end += 1;
                state.selection_start = state.selection_end;
            }
        }

        let prev = state.selection_end;

        unsafe fn is_key_pressed_or_repeat(key: RaylibKey) -> bool {
            unsafe { is_key_pressed(key) || is_key_pressed_repeat(key) }
        }

        if unsafe { is_key_pressed_or_repeat(RaylibKey::Backspace) } {
            if state.selection_start == state.selection_end {
                state.selection_end = state.selection_end.saturating_sub(1);
            }
            if text_edit.len() != 0 {
                for i in (state.selection_end.min(state.selection_start)
                    ..state.selection_end.max(state.selection_start))
                    .rev()
                {
                    text_edit.remove(i);
                }
            }
        }

        if unsafe { is_key_pressed_or_repeat(RaylibKey::Left) } {
            state.selection_end = state.selection_end.saturating_sub(1);
        }

        if unsafe { is_key_pressed_or_repeat(RaylibKey::Right) } {
            state.selection_end += 1;
        }

        state.selection_end =
            usize::clamp(state.selection_end, 0, text_edit.len());
        state.selection_start =
            usize::clamp(state.selection_start, 0, text_edit.len());

        if !unsafe {
            is_key_down(RaylibKey::LeftShift)
                || is_key_down(RaylibKey::RightShift)
        } && prev != state.selection_end
        {
            state.selection_start = state.selection_end;
        }
    } else {
        state.placeholder_is_active |= text_edit.len() == 0;
    }

    let mut text = Vec::<u8, _>::new_in(&scratch);
    for c in text_edit {
        let mut buf = [0u8; 4];
        text.extend(c.encode_utf8(&mut buf).as_bytes());
    }

    if focused == p.id || !state.placeholder_is_active {
        drop(state);
        // NOTE: \0 forces the caret to render if the string is empty
        style.text = aformat!(b.ctx, "{}\0", str::from_utf8(&text).unwrap());
    } else {
        state.original_fg_color = style.fg_color;
        style.text = placeholder.into();
        style.fg_color = 0x00000088;
    }
}

fn update_scroll(b: &ElemBuilder, d: Dim) -> f32 {
    #[derive(Clone, Copy, Debug, Default)]
    struct State {
        target: f32,
    }

    let movement = unsafe { get_mouse_wheel_move()[d as usize] * 50. };

    let mut should_move = false;

    for el in b.ctx.hovered.iter().copied().rev() {
        let elem = b.ctx.elem(1, el);
        let scroll_allowance = elem.scroll_allowance(d);
        let mut can_move =
            scroll_allowance != 0. && b.ctx.elem_state::<State>(el).is_some();
        can_move &= (elem.style.scroll[d] != 0.
            && elem.style.scroll[d] != scroll_allowance)
            || (elem.style.scroll[d] == 0. && movement < 0.)
            || (elem.style.scroll[d] == scroll_allowance && movement > 0.);

        if can_move {
            if el == b.idx && movement != 0. {
                should_move = true;
                break;
            }

            break;
        }
    }

    let p = b.prev();
    let scroll_allowance = p.scroll_allowance(d);
    let state = b.update_state(|s: Option<State>| {
        let mut current = s.unwrap_or_default();
        if should_move {
            current.target -= movement;
        }
        current.target = f32::clamp(current.target, 0., scroll_allowance);
        current
    });

    lerp(p.style.scroll.y, state.target, 0.2)
}

fn render(app: &mut App) {
    let ctx = &mut app.ctx;

    if unsafe { is_mouse_button_pressed(MouseButton::Left) } {
        app.selection = [unsafe { get_mouse_position() }; 2];
        app.focused = Default::default();
    }

    if unsafe { is_mouse_button_down(MouseButton::Left) } {
        app.selection[1] = unsafe { get_mouse_position() };
    }

    {
        let _ss = ctx.push_style(|s| {
            s.margin([4.])
                .padding([4.])
                .font(app.font)
                .font_size(20.)
                .font_spacing(2.)
                .fg_color(BLUE)
                .font_line_spacing(2.)
        });

        let _el = ctx.el(id!()).style(|b, s| {
            s.width(unsafe { get_screen_width() } as f32)
                .height(unsafe { get_screen_height() } as f32)
                .scroll_y(update_scroll(&b, Dim::Y))
                .layout(Flex)
                .dont_wrap(true)
                //.align_y(Align::Center)
                .direction(Top2Bottom)
            //.text(&include_str!("raylib.rs")[..1000 * 3 - 4])
        });

        let _hor = ctx.anon().style(|s| {
            s.direction(Left2Right)
                .width_perc(1.)
                .padding([0.])
                .gap(10.)
                .dont_wrap(true)
        });
        for i in 0..3 {
            let _wrap = ctx.el(id!(#i)).style(|b, s| {
                s.width_perc(0.5)
                    .margin([0.])
                    .height(400.)
                    .bg_color(BLUE)
                    .align_x(Align::Center)
                    .scroll_y(update_scroll(&b, Dim::Y))
                    .clip_overflow(true)
                    .gap(10.)
                    .gap(0.)
            });

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

            for j in 0..3 {
                ctx.el(id!(#i, j)).style(|b, s| {
                    update_focus(b, &mut app.focused);
                    s.fg_color(if app.focused == b.id() { WHITE } else { BLUE })
                        .mutate(|s| update_text(b, s, app.focused, "smh"))
                        .bg_color(if app.focused == b.id() {
                            0x000000ff
                        } else {
                            WHITE
                        })
                        .height(30.)
                        .width(200.)
                });
            }

            for _ in 0..10 {
                let _lup = ctx.anon().style(|s| s.bg_color(GREEN).width(200.));

                ctx.anon().style(|s| {
                    s.text("there is enought text to overflow")
                        .bg_color(WHITE)
                        .width_grow()
                });
            }

            for _ in 0..300 {
                ctx.anon().style(|s| {
                    s.width(1.).bg_color(RED).self_align(Align::Center)
                });
            }

            for _ in 0..3 {
                let _foo =
                    ctx.anon().style(|s| s.width(4. * 30.).bg_color(GREEN));

                for _ in 0..30 {
                    ctx.anon().style(|s| s.width(1.).bg_color(RED));
                }

                ctx.anon().style(|s| s.width_grow().bg_color(RED));
            }
        }
        drop(_hor);

        for i in 0..0 {
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

    let mut active_clip = false;

    for cmd in app.ctx.cmds(
        &mut app.backend,
        Some(&cmds),
        InputState {
            mouse_pos: unsafe { get_mouse_position() },
            selection: Default::default(),
        },
    ) {
        match cmd.data {
            DrawCmdData::Text(TextCmdData {
                x,
                y,
                width: _,
                height,
                size,
                spacing,
                line_spacing,
                content,
                font,
                color,
            }) => {
                let backend = BackendFromBase::new(&mut app.backend);

                let mut pos = None;
                if app.focused == app.ctx.elem(1, cmd.elem).id {
                    if let Some(edit_state) =
                        app.ctx.elem_state::<UpdateTextState>(cmd.elem)
                    {
                        pos = Some((
                            backend.measure_text(
                                font,
                                &app.ctx.text(content)
                                    [..edit_state.selection_start],
                                size,
                                spacing,
                            ),
                            backend.measure_text(
                                font,
                                &app.ctx.text(content)
                                    [..edit_state.selection_end],
                                size,
                                spacing,
                            ),
                        ));
                    }
                }

                if let Some((mut start, mut end)) = pos {
                    [start, end] = [start.min(end), start.max(end)];
                    unsafe {
                        draw_rectangle(
                            RaylibRectangle {
                                x: x + start,
                                y,
                                width: end - start,
                                height,
                            },
                            conv_color(0xffffffaa),
                        )
                    };
                }

                unsafe {
                    draw_text_ex(
                        &*font.0.as_ptr().cast::<IndexedFont>(),
                        &*app.ctx.text(content),
                        [x, y],
                        size,
                        spacing,
                        line_spacing,
                        conv_color(color),
                    );
                }

                if let Some((_, end)) = pos {
                    let rel_offset = unsafe { get_mouse_position()[0] } - x;

                    let index = backend.find_position_index(
                        font,
                        &*app.ctx.text(content),
                        size,
                        spacing,
                        rel_offset,
                    );

                    let state = app
                        .ctx
                        .elem_state_mut::<UpdateTextState>(cmd.elem)
                        .expect("some pos inplies we are some too");

                    if unsafe { is_mouse_button_down(MouseButton::Left) } {
                        state.selection_end = index;
                    }

                    if unsafe { is_mouse_button_pressed(MouseButton::Left) } {
                        state.selection_end = index;
                        state.selection_start = index;
                    }

                    unsafe {
                        draw_rectangle(
                            RaylibRectangle {
                                x: x + end,
                                y,
                                width: spacing.max(1.),
                                height,
                            },
                            conv_color(0xffffffff),
                        );
                    }
                }

                if let Some((_, pos)) = pos {
                    let mut p = app.ctx.elem_mut(1, cmd.elem);
                    p.style.scroll.x = lerp(
                        p.style.scroll.x,
                        f32::clamp(
                            p.style.scroll.x,
                            (pos - p.inner_size(Dim::X) + 10.).max(0.),
                            (pos - 10.).max(0.),
                        ),
                        0.6,
                    );
                }
            }
            DrawCmdData::Rect(RectCmdData { x, y, width, height, color }) => unsafe {
                draw_rectangle(
                    RaylibRectangle { x, y, width, height },
                    conv_color(color),
                );
            },
            DrawCmdData::StartClip(StartClipData { x, y, width, height }) => unsafe {
                active_clip = true;
                begin_scissor_mode(
                    x as i32,
                    y as i32,
                    width as i32,
                    height as i32,
                );
            },
            DrawCmdData::EndClip => {
                active_clip = false;
                unsafe { end_scissor_mode() }
            }
        }
    }

    assert!(!active_clip);

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

    Arna::init_temp_arenas_with_boxes(1024 * 1024);

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
