use {
    crate::Checkpoint,
    alloc::vec::Vec,
    core::{
        cell::{Cell, Ref, RefCell, RefMut},
        fmt::Write,
        mem::transmute,
        ops::{Index, IndexMut},
        ptr::{NonNull, null},
    },
};

pub const DIMS: usize = 2;

#[derive(Default, Debug)]
pub struct DrawCmd {
    pub elem: ElemIdx,
    pub data: DrawCmdData,
}

#[derive(Debug)]
pub struct RectCmdData {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Color,
}

#[derive(Debug)]
pub struct TextCmdData {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub spacing: f32,
    pub content: TextId,
    pub font: FontId,
    pub color: Color,
}

#[derive(Default, Debug)]
pub enum DrawCmdData {
    #[default]
    Null,
    Rect(RectCmdData),
    Text(TextCmdData),
}

#[derive(Default)]
pub struct TextBuf {
    buf: String,
}

impl TextBuf {
    pub fn format(&mut self, args: core::fmt::Arguments) -> TextId {
        let pos = self.buf.len();
        self.buf.write_fmt(args).expect("OOM");
        TextId {
            ptr: null(),
            pos: pos as u32,
            len: (self.buf.len() - pos) as u32,
        }
    }

    pub fn get(&self, id: TextId) -> &str {
        if id.ptr != null() {
            // SAFETY: we are static since pointer is not null
            unsafe {
                let slc = core::slice::from_raw_parts(id.ptr, id.len as usize);
                core::str::from_utf8_unchecked(slc)
            }
        } else {
            &self.buf[id.pos as usize..(id.pos + id.len) as usize]
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct TextId {
    ptr: *const u8,
    pos: u32,
    len: u32,
}

impl From<&'static str> for TextId {
    fn from(value: &'static str) -> Self {
        TextId {
            ptr: value as *const _ as *const _,
            pos: 0,
            len: value.len() as u32,
        }
    }
}

impl TextId {
    pub fn str<'a>(self, buf: &'a TextBuf) -> &'a str {
        buf.get(self)
    }

    pub fn slice(mut self, start: usize, end: usize) -> Self {
        assert!(start <= end);
        assert!(end <= self.len as usize);

        if self.ptr != null() {
            self.ptr = unsafe { self.ptr.add(start) };
        } else {
            self.pos += start as u32;
        }

        self.len = (end - start) as u32;

        self
    }
}

pub struct InputState {
    pub mouse_pos: [f32; DIMS],
}

pub struct Glyph {
    pub offset_x: f32,
    pub advance_x: f32,
    pub width: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct FontId(pub NonNull<()>);

pub trait BackendBase {
    fn get_font_base_size(&mut self, font: FontId) -> f32;
    fn get_glypy_data(&mut self, font: FontId, ch: char) -> Option<Glyph>;

    fn line_height(
        &mut self,
        _: FontId,
        font_size: f32,
        mut line_scaling: f32,
    ) -> f32 {
        if line_scaling == 0. {
            line_scaling = 1.;
        }

        return font_size * line_scaling;
    }

    fn glyph_advance(&mut self, font: FontId, ch: char, font_size: f32) -> f32 {
        let Some(glyph) = self.get_glypy_data(font, ch) else { return 0. };

        let adv;
        if glyph.advance_x > 0. {
            adv = glyph.advance_x;
        } else {
            adv = glyph.width + glyph.offset_x;
        }

        adv * (font_size / self.get_font_base_size(font))
    }
}

pub trait Backend {
    fn line_height(
        &mut self,
        font: FontId,
        font_size: f32,
        line_scaling: f32,
    ) -> f32;

    fn find_line_boundary(
        &mut self,
        font: FontId,
        text: &mut core::str::Chars,
        font_size: f32,
        spacing: f32,
        width: f32,
    ) -> f32;
}

/// NOTE: this is to allow the compiler to optimize the insides of these functions to the
/// fullest extent
#[repr(transparent)]
pub struct BackendFromBase<B>(pub B);

impl<B> BackendFromBase<B> {
    pub fn new(base: &mut B) -> &mut Self {
        unsafe { core::mem::transmute(base) }
    }
}

impl<B: BackendBase> Backend for BackendFromBase<B> {
    fn line_height(
        &mut self,
        font: FontId,
        font_size: f32,
        line_scaling: f32,
    ) -> f32 {
        self.0.line_height(font, font_size, line_scaling)
    }

    fn find_line_boundary(
        &mut self,
        font: FontId,
        text: &mut core::str::Chars,
        font_size: f32,
        spacing: f32,
        width: f32,
    ) -> f32 {
        let mut cursor = 0.;
        let mut last_word_end = 0.;
        let mut last_word_chars = None;
        let mut to_add_spacing = 0.;

        loop {
            let Some(ch) = text.next() else { break };

            if ch.is_whitespace() {
                if ch == '\n' {
                    break;
                } else if cursor > width {
                    match last_word_chars {
                        Some(last_word_chars) => {
                            *text = last_word_chars;
                            return last_word_end;
                        }
                        None => break,
                    }
                } else {
                    last_word_chars = Some(text.clone());
                    last_word_end = cursor;
                }
            }

            let advance = self.0.glyph_advance(font, ch, font_size);
            cursor += to_add_spacing + advance;
            to_add_spacing = spacing;
        }

        cursor
    }
}

#[derive(Default)]
pub struct Ctx {
    frames: [RefCell<FrameCtx>; DIMS],
    parent: Cell<ElemIdx>,
    current_elem: Cell<ElemIdx>,
    style: Cell<Style>,
    pub hovered: Vec<ElemIdx>,
}

impl Ctx {
    pub fn text<'a>(&'a self, id: TextId) -> Ref<'a, str> {
        Ref::map(self.frames[1].borrow(), |v| v.text.get(id))
    }

    pub fn frame<'b>(&'b self, offset: usize) -> RefMut<'b, FrameCtx> {
        self.frames[offset].borrow_mut()
    }

    pub fn elem<'b>(&'b self, frame: usize, id: ElemIdx) -> RefMut<'b, Elem> {
        RefMut::map(self.frames[frame].borrow_mut(), |v| &mut v[id])
    }

    pub fn el(&self, id: impl Into<ElemID>) -> ElemBuilder<'_> {
        let mut frame = self.frame(0);
        let idx = frame.add_elem(Elem {
            id: id.into(),
            parent: self.parent.get(),
            style: self.style.get(),
            ..Default::default()
        });
        frame[self.current_elem.get()].next = idx;
        self.current_elem.set(ElemIdx(0));
        self.parent.set(idx);
        ElemBuilder { ctx: self, idx }
    }

    pub fn push_style(
        &self,
        modifier: impl FnOnce(Style) -> Style,
    ) -> StyleScope<'_> {
        let prev = self.style.get();
        self.style.update(modifier);
        StyleScope { ctx: self, prev }
    }

    pub fn cmds<'a, 'b>(
        &mut self,
        backend: &mut dyn Backend,
        scratch: &'a Checkpoint<'b>,
        input_state: InputState,
    ) -> Vec<DrawCmd, &'a Checkpoint<'b>> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Order {
            Pre,
            Post,
        }

        fn traverse(
            frame: &mut FrameCtx,
            root: ElemIdx,
            order: Order,
            action: &mut dyn FnMut(&mut FrameCtx, ElemIdx),
        ) {
            if order == Order::Pre {
                action(frame, root);
            }

            let mut iter = frame[root].first_child;
            while let Some(child) = iter.next(frame) {
                traverse(frame, child, order, action);
            }

            if order == Order::Post {
                action(frame, root);
            }
        }

        #[derive(Default, Clone, Copy)]
        struct Extra {
            used_y: f32,
            used_x: f32,
            line: u16,
        }

        // TODO: after further use this will be a macro
        #[repr(transparent)]
        struct ExtraArr([Extra]);

        impl IndexMut<ElemIdx> for ExtraArr {
            fn index_mut(&mut self, index: ElemIdx) -> &mut Self::Output {
                &mut self.0[index.0 as usize]
            }
        }

        impl Index<ElemIdx> for ExtraArr {
            type Output = Extra;

            fn index(&self, index: ElemIdx) -> &Self::Output {
                &self.0[index.0 as usize]
            }
        }

        impl ExtraArr {
            fn from(value: &mut [Extra]) -> &mut ExtraArr {
                unsafe { transmute(value) }
            }
        }

        let arna = crate::Arna::scratch(scratch);

        self.current_elem.take();
        self.parent.take();
        self.frames.swap(0, 1);
        self.frames[0].get_mut().elemets.truncate(1);
        self.frames[0].get_mut().text.buf.clear();

        let frame = self.frames[1].get_mut();

        for elem in &mut frame.elemets {
            for (sdim, edim) in
                elem.style.dims.iter_mut().zip(elem.dims.iter_mut())
            {
                if let Size::Fixed(size) = sdim.size.to_size() {
                    edim.size = size;
                }
                edim.size = f32::max(edim.size, sdim.min_size);
                edim.size = f32::max(edim.size, sdim.padding_sum());
            }
        }

        let root = ElemIdx(1);
        let extra = ExtraArr::from(arna.alloc_default(frame.elemets.len()));

        fn get_text_params(
            ctx: &FrameCtx,
            root: ElemIdx,
        ) -> Option<(FontId, &str)> {
            let text = ctx[root].style.text.str(&ctx.text);
            if let Some(font) = ctx[root].style.font
                && text != ""
            {
                Some((font, text))
            } else {
                None
            }
        }

        fn measure_text(
            ctx: &FrameCtx,
            backend: &mut dyn Backend,
            root: ElemIdx,
            max_width: f32,
        ) -> [f32; 2] {
            measure_text_ext(ctx, backend, root, max_width, &mut |_, _, _| {})
        }

        fn measure_text_ext(
            ctx: &FrameCtx,
            backend: &mut dyn Backend,
            root: ElemIdx,
            mut max_width: f32,
            with_chunk: &mut dyn FnMut(TextId, f32, f32),
        ) -> [f32; 2] {
            if max_width == 0. {
                max_width = ctx[root].inner_size(0)
            }

            if let Some((font, full_text)) = get_text_params(ctx, root) {
                let mut text = full_text.chars();
                let mut width = 0.;
                let mut height = 0.;

                let line_height = backend.line_height(
                    font,
                    ctx[root].style.font_size,
                    ctx[root].style.line_height_mult,
                );

                while text.as_str().len() != 0 {
                    let prev_len = text.as_str().len();
                    let line_width = backend.find_line_boundary(
                        font,
                        &mut text,
                        ctx[root].style.font_size,
                        ctx[root].style.font_spacing,
                        max_width,
                    );
                    width = line_width.max(width);
                    let curr_len = text.as_str().len();

                    assert!(curr_len < prev_len, "{:?}", &text.as_str()[..10]);

                    with_chunk(
                        ctx[root].style.text.slice(
                            full_text.len() - prev_len,
                            full_text.len() - curr_len,
                        ),
                        height,
                        line_width,
                    );

                    height += line_height;
                }

                [width, height]
            } else {
                [0., 0.]
            }
        }

        traverse(frame, root, Order::Post, &mut |ctx, root| match ctx[root]
            .style
            .layout
        {
            Layout::Flex => {
                let [x, _] = ctx[root].style.direction.dims();
                if !ctx[root].style.dims[x].size.is_fixed() {
                    let mut iter = ctx[root].first_child;
                    let mut gap_x = 0.;
                    while let Some(child) = iter.next(ctx) {
                        ctx[root].dims[x].size +=
                            gap_x + ctx[child].outer_size(x);
                        gap_x = ctx[root].style.gap;
                    }
                }

                for (d, s) in measure_text(ctx, backend, root, f32::MAX)
                    .into_iter()
                    .enumerate()
                {
                    if ctx[root].style.dims[d].size.is_fit() {
                        ctx[root].dims[d].size = f32::max(
                            ctx[root].dims[d].size,
                            s + ctx[root].style.dims[d].padding_sum(),
                        );
                    }
                }
            }
        });

        traverse(frame, root, Order::Pre, &mut |ctx, root| {
            let [x, y] = ctx[root].style.direction.dims();
            match ctx[root].style.layout {
                Layout::Flex => {
                    let mut line = 0u16;
                    let mut line_first = ctx[root].first_child;
                    let mut line_x = 0.;
                    let mut iter = line_first;
                    let mut gap_x = 0.;
                    while let Some(child) = iter.next(ctx) {
                        if let Size::Perc(p) =
                            ctx[child].style.dims[y].size.to_size()
                        {
                            ctx[child].dims[y].size = (ctx[root].inner_size(y)
                                - ctx[child].style.dims[y].margin_sum())
                                * p;

                            if y == 0 {
                                let [_, h] =
                                    measure_text(ctx, backend, child, 0.);

                                ctx[child].dims[x].size = f32::max(
                                    ctx[child].dims[x].size,
                                    h + ctx[child].style.dims[x].padding_sum(),
                                );
                            }
                        }

                        line_x += gap_x + ctx[child].outer_size(x);
                        gap_x = ctx[root].style.gap;
                        let mut free_space = ctx[root].inner_size(x) - line_x;

                        extra[child].line = line;

                        // NOTE: we look ahead so this means we are the first in the row
                        if free_space < 0. {
                            ctx[child].dims[x].size += free_space;
                        }

                        let mut process_line = iter.0 == 0;
                        if !process_line {
                            process_line = line_x
                                + ctx[root].style.gap
                                + ctx[iter].outer_size(x)
                                > ctx[root].inner_size(x);
                        }

                        if process_line {
                            line += 1;

                            let mut total_perc = 0.;

                            let mut line_iter = line_first;
                            while let Some(line_child) = line_iter.next(ctx)
                                && line_child != iter
                            {
                                if let Size::Perc(p) =
                                    ctx[line_child].style.dims[x].size.to_size()
                                {
                                    free_space += ctx[line_child].inner_size(x);
                                    total_perc += p;
                                }
                            }

                            let mut line_iter = line_first;
                            while let Some(line_child) = line_iter.next(ctx)
                                && line_child != iter
                            {
                                if let Size::Perc(p) =
                                    ctx[line_child].style.dims[x].size.to_size()
                                {
                                    ctx[line_child].dims[x].size = f32::max(
                                        ctx[line_child].dims[x].size,
                                        free_space * (p / total_perc)
                                            + ctx[line_child].style.dims[x]
                                                .padding_sum(),
                                    );
                                }
                            }

                            line_x = 0.;
                            gap_x = 0.;
                            line_first = iter;
                        }
                    }
                }
            }
        });

        traverse(frame, root, Order::Post, &mut |ctx, root| {
            let [x, y] = ctx[root].style.direction.dims();

            match ctx[root].style.layout {
                Layout::Flex => {
                    let mut line_x = 0.;
                    let mut line_y = 0.;
                    let mut max_x = 0.;
                    let mut max_y = 0.;
                    let mut first_child = ctx[root].first_child;
                    let mut iter = first_child;
                    let mut gap_x = 0.;
                    let mut gap_y = 0.;
                    while let Some(child) = iter.next(ctx) {
                        line_x += gap_x + ctx[child].outer_size(x);
                        gap_x = ctx[root].style.gap;
                        max_x = f32::max(max_x, line_x);
                        line_y = f32::max(line_y, ctx[child].outer_size(y));

                        let mut process_line = iter.0 == 0;
                        if !process_line {
                            process_line = line_x
                                + ctx[root].style.gap
                                + ctx[iter].outer_size(x)
                                > ctx[root].inner_size(x);
                        }

                        if process_line {
                            extra[first_child].used_x = line_x;
                            max_y += gap_y + line_y;
                            gap_y = ctx[root].style.gap;
                            line_x = 0.;
                            line_y = 0.;
                            gap_x = 0.;
                            first_child = iter;
                        }
                    }

                    extra[root].used_y = max_y;

                    for ((d, max), td) in [x, y]
                        .into_iter()
                        .zip([max_x, max_y])
                        .zip(measure_text(ctx, backend, root, 0.))
                    {
                        if ctx[root].style.dims[d].size.is_fit() {
                            ctx[root].dims[d].size = f32::max(max, td)
                                + ctx[root].style.dims[d].padding_sum();
                        }
                    }
                }
            }
        });

        traverse(frame, root, Order::Pre, &mut |ctx, root| {
            let [x, y] = ctx[root].style.direction.dims();

            match ctx[root].style.layout {
                Layout::Flex => {
                    let free_y = ctx[root].inner_size(y) - extra[root].used_y;

                    let mut iter = ctx[root].first_child;
                    let mut cursor_x = 0.;
                    let mut cursor_y = ctx[root].dims[y].pos
                        + ctx[root].style.dims[y].align.offset(free_y)
                        + ctx[root].style.dims[y].padding[0];
                    let mut y_size = 0.;
                    let mut last_line = u16::MAX;
                    let mut gap_x = 0.;
                    let mut gap_y = 0.;
                    while let Some(child) = iter.next(ctx) {
                        if extra[child].line != last_line {
                            cursor_x = ctx[root].dims[x].pos
                                + ctx[root].style.dims[x].align.offset(
                                    (ctx[root].inner_size(x)
                                        - extra[child].used_x)
                                        .abs(),
                                )
                                + ctx[root].style.dims[x].padding[0];
                            cursor_y += gap_y + y_size;
                            gap_y = ctx[root].style.gap;
                            y_size = 0.;
                            gap_x = 0.;
                            last_line = extra[child].line;

                            let mut iter = child;
                            while let Some(line_child) = iter.next(ctx)
                                && extra[line_child].line == last_line
                            {
                                y_size = f32::max(
                                    y_size,
                                    ctx[line_child].outer_size(y),
                                );
                            }
                        }

                        ctx[child].dims[x].pos = gap_x
                            + cursor_x
                            + ctx[child].style.dims[x].margin[0];
                        ctx[child].dims[y].pos = cursor_y
                            + ctx[child].style.dims[y].margin[0]
                            + ctx[child]
                                .style
                                .self_align
                                .offset(y_size - ctx[child].outer_size(y));
                        cursor_x += gap_x + ctx[child].outer_size(x);
                        gap_x = ctx[root].style.gap;
                    }
                }
            }
        });

        let mut order = Vec::with_capacity_in(frame.elemets.len(), &arna);

        traverse(frame, root, Order::Pre, &mut |_, root| order.push(root));

        self.hovered.clear();

        let mut buf = Vec::new_in(scratch);
        for &n in &order {
            let node = frame[n];

            // NOTE: this will collect all overlaps, the last overlap has the highest
            // priority since is't the top most element.
            if node
                .dims
                .into_iter()
                .zip(input_state.mouse_pos)
                .all(|(d, m)| d.pos <= m && m <= d.pos + d.size)
            {
                self.hovered.push(n);
            }

            if node.style.bg_color != 0 {
                buf.push(DrawCmd {
                    elem: n,
                    data: DrawCmdData::Rect(RectCmdData {
                        x: node.dims[0].pos,
                        y: node.dims[1].pos,
                        width: node.dims[0].size,
                        height: node.dims[1].size,
                        color: node.style.bg_color,
                    }),
                });
            }

            if let Some((font, _)) = get_text_params(frame, n) {
                let [_, y] = measure_text(frame, backend, n, 0.);
                measure_text_ext(
                    frame,
                    backend,
                    n,
                    0.,
                    &mut |content, y_off, width| {
                        buf.push(DrawCmd {
                            elem: n,
                            data: DrawCmdData::Text(TextCmdData {
                                x: node.dims[0].pos
                                    + node.style.dims[0].padding[0]
                                    + node.style.dims[0]
                                        .align
                                        .offset(node.inner_size(0) - width),
                                y: node.dims[1].pos
                                    + node.style.dims[1].padding[0]
                                    + node.style.dims[1]
                                        .align
                                        .offset(node.inner_size(1) - y)
                                    + y_off,
                                size: frame[n].style.font_size,
                                spacing: frame[n].style.font_spacing,
                                content,
                                font,
                                color: frame[n].style.fg_color,
                            }),
                        });
                    },
                );
            }
        }

        buf
    }
}

pub struct StyleScope<'b> {
    pub ctx: &'b Ctx,
    pub prev: Style,
}

impl Drop for StyleScope<'_> {
    fn drop(&mut self) {
        self.ctx.style.set(self.prev);
    }
}

pub struct FrameCtx {
    elemets: Vec<Elem>,
    text: TextBuf,
}

impl Default for FrameCtx {
    fn default() -> Self {
        Self { elemets: vec![Default::default()], text: Default::default() }
    }
}

impl core::fmt::Display for FrameCtx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        fn display_elem(
            s: &FrameCtx,
            f: &mut core::fmt::Formatter<'_>,
            depht: usize,
            root: ElemIdx,
        ) -> core::fmt::Result {
            let elem = s[root];
            write!(
                f,
                "({:8x}, {} {}x{} {}x{}/{}x{}) {{",
                elem.id.0,
                elem.style.text.str(&s.text),
                elem.style.dims[0].size,
                elem.style.dims[1].size,
                elem.dims[0].pos,
                elem.dims[1].pos,
                elem.dims[0].size,
                elem.dims[1].size,
            )?;

            if elem.first_child.0 != 0 {
                writeln!(f)?;

                let mut iter = elem.first_child;
                while let Some(child) = iter.next(s) {
                    for _ in 0..depht + 1 {
                        write!(f, " ")?;
                    }
                    display_elem(s, f, depht + 1, child)?;
                }

                for _ in 0..depht {
                    write!(f, " ")?;
                }
            }
            writeln!(f, "}}")?;

            Ok(())
        }

        display_elem(self, f, 0, ElemIdx(0))
    }
}

impl IndexMut<ElemIdx> for FrameCtx {
    fn index_mut(&mut self, index: ElemIdx) -> &mut Self::Output {
        &mut self.elemets[index.0 as usize]
    }
}

impl Index<ElemIdx> for FrameCtx {
    type Output = Elem;

    fn index(&self, index: ElemIdx) -> &Self::Output {
        &self.elemets[index.0 as usize]
    }
}

impl FrameCtx {
    pub fn add_elem(&mut self, elem: Elem) -> ElemIdx {
        self.elemets.push(elem);
        ElemIdx((self.elemets.len() - 1) as u16)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct ElemID(pub u32);

impl From<&'_ str> for ElemID {
    fn from(value: &'_ str) -> Self {
        Self(fnv1a(value.as_bytes()))
    }
}

impl From<i32> for ElemID {
    fn from(value: i32) -> Self {
        Self(value as u32)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct ElemIdx(pub u16);

pub struct ElemBuilder<'b> {
    pub ctx: &'b Ctx,
    pub idx: ElemIdx,
}

impl<'b> ElemBuilder<'b> {
    pub fn format(&self, args: core::fmt::Arguments) -> TextId {
        self.ctx.frame(0).text.format(args)
    }

    pub fn style(self, modifier: impl FnOnce(&Self, Style) -> Style) -> Self {
        let mut style = self.ctx.elem(0, self.idx).style;
        style = modifier(&self, style);
        self.ctx.elem(0, self.idx).style = style;

        self
    }

    pub fn idx(self, value: usize) -> Self {
        let mut elem = self.ctx.elem(0, self.idx);
        elem.id.0 = mix_u32(elem.id.0, value as u32);
        self
    }

    pub fn hovered(&self) -> bool {
        self.ctx.hovered.last().map_or(false, |&n| {
            self.ctx.elem(1, n).id == self.ctx.elem(0, self.idx).id
        })
    }

    /// This is ture for any element that overlaps with mouse
    pub fn any_hovered(&self) -> bool {
        let curr = self.ctx.elem(0, self.idx);
        self.ctx.hovered.iter().any(|&n| self.ctx.elem(1, n).id == curr.id)
    }

    pub fn scope<T>(self, content: impl FnOnce(Self) -> T) -> T {
        content(self)
    }
}

impl Drop for ElemBuilder<'_> {
    fn drop(&mut self) {
        self.ctx.parent.set(self.ctx.elem(0, self.idx).parent);
        let mut parent = self.ctx.elem(0, self.ctx.parent.get());
        if parent.first_child.0 == 0 {
            parent.first_child = self.idx;
        }
        self.ctx.current_elem.set(self.idx);
    }
}

#[derive(Clone, Copy, Default)]
pub struct Elem {
    pub id: ElemID,
    parent: ElemIdx,
    first_child: ElemIdx,
    next: ElemIdx,

    pub style: Style,

    dims: [LayoutDim; DIMS],
}

impl Elem {
    pub fn inner_size(&self, dim: usize) -> f32 {
        self.dims[dim].size - self.style.dims[dim].padding_sum()
    }

    pub fn outer_size(&self, dim: usize) -> f32 {
        self.dims[dim].size + self.style.dims[dim].margin_sum()
    }
}

pub enum Size {
    Fixed(f32),
    Perc(f32),
    FitChildren,
}

pub trait ToSize: Sized {
    fn to_size(self) -> Size;

    fn is_fixed(self) -> bool {
        matches!(self.to_size(), Size::Fixed(_))
    }

    fn is_fit(self) -> bool {
        matches!(self.to_size(), Size::FitChildren)
    }
}

impl ToSize for f32 {
    fn to_size(self) -> Size {
        if self > 0. {
            Size::Fixed(self)
        } else if self == 0. {
            Size::FitChildren
        } else {
            Size::Perc(-self)
        }
    }
}

pub static GROW: f32 = perc(1.);

pub const fn perc(vl: f32) -> f32 {
    debug_assert!(vl >= 0.0);
    -vl
}

#[derive(Clone, Copy, Default)]
struct LayoutDim {
    pos: f32,
    size: f32,
}

impl ElemIdx {
    pub fn next(&mut self, ctx: &FrameCtx) -> Option<ElemIdx> {
        if self.0 == 0 {
            return None;
        }
        let res = *self;
        *self = ctx[*self].next;
        Some(res)
    }
}

#[derive(Clone, Copy, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

impl Align {
    pub fn offset(self, free: f32) -> f32 {
        free * match self {
            Align::Start => 0.,
            Align::Center => 0.5,
            Align::End => 1.,
        }
    }
}

macro_rules! derive_style_builder {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            pub dims: [StyleDim; DIMS],

            $(
                $(#[$field_mod:ident])?
                $field_vis:vis $field_name:ident: $field_type:ty,
            )*
        }
    ) => {

        $(#[$meta])*
        $vis struct $name {
            pub dims: [StyleDim; DIMS],

            $(
                $field_vis $field_name: $field_type,
            )*
        }

        impl $name {
            $(
                $field_vis fn $field_name(mut self, value:
                    derive_style_builder!(@$($field_mod)? $field_type)) -> Self {
                    self.$field_name = value.into();
                    self
                }
            )*
        }
    };

    (@into $field_type:ty) => {impl Into<$field_type>};
    (@ $field_type:ty) => {$field_type};
}

derive_style_builder! {
    #[derive(Clone, Copy, Default)]
    pub struct Style {
        pub dims: [StyleDim; DIMS],
        pub bg_color: Color,
        pub fg_color: Color,
        pub layout: Layout,
        pub direction: Direction,
        pub dont_wrap: bool,
        #[into]
        pub text: TextId,
        pub self_align: Align,
        pub gap: f32,
        #[into]
        pub font: Option<FontId>,
        pub font_size: f32,
        pub font_spacing: f32,
        pub line_height_mult: f32,
    }
}

impl Style {
    pub fn align_x(mut self, value: Align) -> Self {
        self.dims[0].align = value;
        self
    }

    pub fn align_y(mut self, value: Align) -> Self {
        self.dims[1].align = value;
        self
    }

    /// you can pass [padding_left, padding_top, padding_bottom, padding_right],
    /// [padding_x, padding_y], [padding]
    pub fn padding<const SIZE: usize>(mut self, values: [f32; SIZE]) -> Self {
        self.dims[0].padding = [values[0 % SIZE], values[2 % SIZE]];
        self.dims[1].padding = [values[1 % SIZE], values[3 % SIZE]];
        self
    }

    /// you can pass [margin_left, margin_top, margin_bottom, margin_right],
    /// [margin_x, margin_y], [margin]
    pub fn margin<const SIZE: usize>(mut self, values: [f32; SIZE]) -> Self {
        self.dims[0].margin = [values[0 % SIZE], values[2 % SIZE]];
        self.dims[1].margin = [values[1 % SIZE], values[3 % SIZE]];
        self
    }

    pub fn min_width_px(mut self, value: usize) -> Self {
        self.dims[0].min_size = value as f32;
        self
    }

    pub fn width_px(mut self, value: usize) -> Self {
        self.dims[0].size = value as f32;
        self
    }

    pub fn width_fit(mut self) -> Self {
        self.dims[0].size = 0.;
        self
    }

    pub fn width_perc(mut self, value: f32) -> Self {
        self.dims[0].size = perc(value);
        self
    }

    pub fn width_grow(mut self) -> Self {
        self.dims[0].size = -1.;
        self
    }

    pub fn height_px(mut self, value: usize) -> Self {
        self.dims[1].size = value as f32;
        self
    }

    pub fn height_perc(mut self, value: f32) -> Self {
        self.dims[1].size = perc(value);
        self
    }

    pub fn height_grow(mut self) -> Self {
        self.dims[1].size = -1.;
        self
    }

    pub fn height_fit(mut self) -> Self {
        self.dims[1].size = 0.;
        self
    }

    pub fn gap_px(mut self, value: usize) -> Self {
        self.gap = value as f32;
        self
    }
}

#[derive(Clone, Copy, Default)]
pub struct StyleDim {
    pub align: Align,
    pub size: f32,
    pub min_size: f32,
    pub padding: [f32; 2],
    pub margin: [f32; 2],
}

impl StyleDim {
    pub fn padding_sum(&self) -> f32 {
        self.padding[0] + self.padding[1]
    }

    pub fn margin_sum(&self) -> f32 {
        self.margin[0] + self.margin[1]
    }
}

pub type Color = u32;

pub const RED: Color = 0xff0000ff;
pub const GREEN: Color = 0x00ff00ff;
pub const BLUE: Color = 0x0000ffff;
pub const WHITE: Color = 0xffffffff;

pub const fn fnv1a(bytes: &[u8]) -> u32 {
    const OFFSET_BASIS: u32 = 0x811c9dc5;
    const PRIME: u32 = 0x01000193;

    let mut hash = OFFSET_BASIS;
    let mut i = 0;

    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(PRIME);
        i += 1;
    }

    hash
}

pub const fn mix_u32(a: u32, b: u32) -> u32 {
    let mut h = a ^ b.wrapping_mul(0x9e3779b9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x85ebca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^= h >> 16;
    h
}

#[derive(Clone, Copy, Default)]
pub enum Layout {
    #[default]
    Flex,
}

#[derive(Clone, Copy, Default)]
pub enum Direction {
    #[default]
    Left2Right,
    Top2Bottom,
}

impl Direction {
    pub fn dims(self) -> [usize; 2] {
        match self {
            Direction::Left2Right => [0, 1],
            Direction::Top2Bottom => [1, 0],
        }
    }
}
