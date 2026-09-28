use {
    crate::{Checkpoint, SimdIter, simd},
    alloc::{string::String, vec::Vec},
    core::{
        cell::{Cell, Ref, RefCell, RefMut},
        fmt::Write,
        mem::transmute,
        ops::{Index, IndexMut},
        ptr::{NonNull, null},
        slice,
    },
};

pub const DIMS: usize = 2;

/// Shorthand for computing an element id at compiletime
#[macro_export]
macro_rules! id {
    () => { const { id!(file!(), line!(), column!()) } };
    ($expr:expr $(, $idx:expr)*) => {
        const { $crate::imui::ElemID($crate::imui::fnv1a(str::as_bytes($expr)))
             } $(.idx($idx as usize))*
    };
}

#[derive(Debug, Clone, Copy)]
pub struct DrawCmd {
    pub elem: ElemIdx,
    pub data: DrawCmdData,
}

#[derive(Debug, Clone, Copy)]
pub struct RectCmdData {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Color,
}

#[derive(Debug, Clone, Copy)]
pub struct TextCmdData {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub spacing: f32,
    pub line_spacing: f32,
    pub content: TextId,
    pub font: FontId,
    pub color: Color,
}

#[derive(Debug, Clone, Copy)]
pub enum DrawCmdData {
    Rect(RectCmdData),
    Text(TextCmdData),
}

#[derive(Default, Debug)]
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

#[derive(Default, Debug, Clone, Copy)]
pub struct InputState {
    pub mouse_pos: [f32; DIMS],
}

#[derive(Default, Debug, Clone, Copy)]
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

#[derive(Default, Debug)]
pub struct Ctx {
    frames: [RefCell<FrameCtx>; DIMS],
    parent: Cell<ElemIdx>,
    current_elem: Cell<ElemIdx>,
    style: Cell<Style>,
    elem_index: Vec<u8>,
    elem_index_ids: Vec<ElemIdx>,
    pub hovered: Vec<ElemIdx>,
    pub measure_cache: MeasureCache,
}

impl Ctx {
    pub fn format(&self, args: core::fmt::Arguments) -> TextId {
        self.frame(0).text.format(args)
    }

    pub fn text<'a>(&'a self, id: TextId) -> Ref<'a, str> {
        Ref::map(self.frames[1].borrow(), |v| v.text.get(id))
    }

    pub fn frame<'b>(&'b self, offset: usize) -> RefMut<'b, FrameCtx> {
        self.frames[offset].borrow_mut()
    }

    pub fn elem_by_id<'b>(&'b self, id: ElemID) -> RefMut<'b, Elem> {
        self.elem_mut(1, self.elem_idx_by_id(id))
    }

    pub fn elem_idx_by_id(&self, id: ElemID) -> ElemIdx {
        let frame = self.frame(1);
        SimdIter::new(&self.elem_index[..], id.hash())
            .map(|idx| self.elem_index_ids[idx])
            .find(|&i| frame[i].id == id)
            .unwrap_or_default()
    }

    pub fn elem<'b>(&'b self, frame: usize, id: ElemIdx) -> Ref<'b, Elem> {
        Ref::map(self.frames[frame].borrow(), |v| &v[id])
    }

    pub fn elem_mut<'b>(
        &'b self,
        frame: usize,
        id: ElemIdx,
    ) -> RefMut<'b, Elem> {
        RefMut::map(self.frames[frame].borrow_mut(), |v| &mut v[id])
    }

    pub fn anon(&self) -> AnonElemBuilder<'_> {
        AnonElemBuilder(self.el(ElemID(0)))
    }

    /// If you don't need to access the builder during the styling, prefer `.anon()`
    pub fn el(&self, id: ElemID) -> ElemBuilder<'_> {
        let mut frame = self.frame(0);
        let idx = frame.add_elem(Elem {
            id,
            parent: self.parent.get(),
            style: self.style.get(),
            ..Default::default()
        });
        frame[self.current_elem.get()].next = idx;
        self.current_elem.set(ElemIdx(0));
        self.parent.set(idx);
        ElemBuilder { ctx: self, idx, prev_id: Default::default() }
    }

    pub fn push_style(
        &self,
        modifier: impl FnOnce(Style) -> Style,
    ) -> StyleScope<'_> {
        let prev = self.style.get();
        self.style.update(modifier);
        StyleScope { ctx: self, prev }
    }

    #[cfg(feature = "std")]
    pub fn cmds<'a>(
        &mut self,
        backend: &mut impl BackendBase,
        scratch: &'a Checkpoint,
        input_state: InputState,
    ) -> &'a mut [DrawCmd] {
        let arna = crate::Arna::scratch(scratch);
        self.cmds_ext(
            BackendFromBase::new(backend),
            scratch,
            &arna,
            input_state,
        )
    }

    pub fn cmds_ext<'a>(
        &mut self,
        backend: &mut dyn Backend,
        scratch: &'a Checkpoint,
        arna: &Checkpoint,
        input_state: InputState,
    ) -> &'a mut [DrawCmd] {
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
            text_hash: u32,
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
            extra: &ExtraArr,
            cache: &mut MeasureCache,
            backend: &mut dyn Backend,
            root: ElemIdx,
            max_width: f32,
        ) -> [f32; DIMS] {
            let node = &ctx[root];

            cache.get_or_insert(
                node.style.text.len,
                MeasureCacheKey {
                    text_hash: extra[root].text_hash,
                    elem_id: node.id,
                    max_width,
                },
                &mut || {
                    measure_text_ext(
                        ctx,
                        backend,
                        root,
                        max_width,
                        &mut |_, _, _| {},
                    )
                },
            )
        }

        fn measure_text_ext(
            ctx: &FrameCtx,
            backend: &mut dyn Backend,
            root: ElemIdx,
            mut max_width: f32,
            with_chunk: &mut dyn FnMut(TextId, f32, f32),
        ) -> [f32; DIMS] {
            let r = &ctx[root];

            if max_width == 0. {
                max_width = r.inner_size(Dim::X)
            }

            if let Some((font, full_text)) = get_text_params(ctx, root) {
                let mut text = full_text.chars();
                let mut width = 0.;
                let mut height = 0.;

                let line_height = r.style.font_size;
                let mut line_spacing = 0.;

                while text.as_str().len() != 0 {
                    let prev_len = text.as_str().len();
                    let line_width = backend.find_line_boundary(
                        font,
                        &mut text,
                        r.style.font_size,
                        r.style.font_spacing,
                        max_width,
                    );
                    width = line_width.max(width);
                    let curr_len = text.as_str().len();

                    assert!(curr_len < prev_len, "{:?}", &text.as_str()[..10]);

                    with_chunk(
                        r.style.text.slice(
                            full_text.len() - prev_len,
                            full_text.len() - curr_len,
                        ),
                        height,
                        line_width,
                    );

                    height += line_spacing + line_height;
                    line_spacing = r.style.font_line_spacing;
                }

                [width, height]
            } else {
                [0., 0.]
            }
        }

        self.current_elem.take();
        self.parent.take();
        self.elem_index.clear();
        self.elem_index_ids.clear();
        self.frames.swap(0, 1);
        self.frames[0].get_mut().elemets.truncate(1);
        self.frames[0].get_mut().text.buf.clear();

        let ctx = self.frames[1].get_mut();
        let extra =
            ExtraArr::from(arna.alloc_default(ctx.elemets.len()).unwrap());

        for (i, (elem, extra)) in
            ctx.elemets.iter_mut().zip(&mut extra.0).enumerate()
        {
            if elem.id.0 != 0 {
                self.elem_index.push(elem.id.hash());
                self.elem_index_ids.push(ElemIdx(i as u32));
            }

            extra.text_hash = fnv1a(ctx.text.get(elem.style.text).as_bytes());

            for dim in [Dim::X, Dim::Y] {
                if let Size::Fixed(size) = elem.style.size[dim].to_size() {
                    elem.size[dim] = size;
                }
                elem.size[dim] =
                    f32::max(elem.size[dim], elem.style.min_size[dim]);
                elem.size[dim] =
                    f32::max(elem.size[dim], elem.style.padding[dim].sum());
            }
        }

        self.elem_index.resize(simd::align_forward(self.elem_index.len()), 0);

        macro_rules! measure_text {
            ($ctx:expr, $node:expr, $width:expr) => {
                measure_text(
                    $ctx,
                    &extra,
                    &mut self.measure_cache,
                    backend,
                    $node,
                    $width,
                )
            };
        }

        let root = ElemIdx(1);

        traverse(ctx, root, Order::Post, &mut |ctx, root| match ctx[root]
            .style
            .layout
        {
            Layout::Flex => {
                let [x, _] = ctx[root].style.direction.dims();
                if !ctx[root].style.size[x].is_fixed() {
                    let mut iter = ctx[root].first_child;
                    let mut gap_x = 0.;
                    while let Some(child) = iter.next(ctx) {
                        ctx[root].size[x] += gap_x + ctx[child].outer_size(x);
                        gap_x = ctx[root].style.gap;
                    }
                }

                for (d, s) in [Dim::X, Dim::Y].into_iter().zip(measure_text!(
                    ctx,
                    root,
                    f32::MAX
                )) {
                    if ctx[root].style.size[d].is_fit() {
                        ctx[root].size[d] = f32::max(
                            ctx[root].size[d],
                            s + ctx[root].style.padding[d].sum(),
                        );
                    }
                }
            }
        });

        traverse(ctx, root, Order::Pre, &mut |ctx, root| {
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
                            ctx[child].style.size[y].to_size()
                        {
                            ctx[child].size[y] = (ctx[root].inner_size(y)
                                - ctx[child].style.margin[y].sum())
                                * p;

                            if y == Dim::X {
                                let [_, h] = measure_text!(ctx, child, 0.);

                                ctx[child].size[x] = f32::max(
                                    ctx[child].size[x],
                                    h + ctx[child].style.padding[x].sum(),
                                );
                            }
                        }

                        line_x += gap_x + ctx[child].outer_size(x);
                        gap_x = ctx[root].style.gap;
                        let mut free_space = ctx[root].inner_size(x) - line_x;

                        extra[child].line = line;

                        // NOTE: we look ahead so this means we are the first in the row
                        if free_space < 0. && !ctx[child].size[x].is_fixed() {
                            ctx[child].size[x] += free_space;
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
                                    ctx[line_child].style.size[x].to_size()
                                {
                                    free_space += ctx[line_child].inner_size(x);
                                    total_perc += p;
                                }
                            }

                            total_perc = total_perc.max(1.);

                            let mut line_iter = line_first;
                            while let Some(line_child) = line_iter.next(ctx)
                                && line_child != iter
                            {
                                if let Size::Perc(p) =
                                    ctx[line_child].style.size[x].to_size()
                                {
                                    ctx[line_child].size[x] = f32::max(
                                        ctx[line_child].size[x],
                                        free_space * (p / total_perc)
                                            + ctx[line_child].style.padding[x]
                                                .sum(),
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

        traverse(ctx, root, Order::Post, &mut |ctx, root| {
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
                        .zip(measure_text!(ctx, root, 0.))
                    {
                        if ctx[root].style.size[d].is_fit() {
                            ctx[root].size[d] = f32::max(max, td)
                                + ctx[root].style.padding[d].sum();
                        }
                    }
                }
            }
        });

        traverse(ctx, root, Order::Pre, &mut |ctx, root| {
            let [x, y] = ctx[root].style.direction.dims();

            match ctx[root].style.layout {
                Layout::Flex => {
                    let free_y = ctx[root].inner_size(y) - extra[root].used_y;

                    let mut iter = ctx[root].first_child;
                    let mut cursor_x = 0.;
                    let mut cursor_y = ctx[root].pos[y]
                        + ctx[root].style.align[y].offset(free_y)
                        + ctx[root].style.padding[y].before;
                    let mut y_size = 0.;
                    let mut last_line = u16::MAX;
                    let mut gap_x = 0.;
                    let mut gap_y = 0.;
                    while let Some(child) = iter.next(ctx) {
                        if extra[child].line != last_line {
                            cursor_x = ctx[root].pos[x]
                                + ctx[root].style.align[x].offset(
                                    ctx[root].inner_size(x)
                                        - extra[child].used_x,
                                )
                                + ctx[root].style.padding[x].before;
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

                        ctx[child].pos[x] = gap_x
                            + cursor_x
                            + ctx[child].style.margin[x].before;
                        ctx[child].pos[y] = cursor_y
                            + ctx[child].style.margin[y].before
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

        let mut order = Vec::with_capacity_in(ctx.elemets.len(), &arna);

        traverse(ctx, root, Order::Pre, &mut |_, root| order.push(root));

        order.sort_by_key(|&v| ctx[v].style.layer);

        self.hovered.clear();

        let mut buf = Vec::new_in(scratch);
        for &n in &order {
            let node = ctx[n];

            // NOTE: this will collect all overlaps, the last overlap has the highest
            // priority since is't the top most element.
            if [Dim::X, Dim::Y].into_iter().zip(input_state.mouse_pos).all(
                |(d, m)| node.pos[d] <= m && m <= node.pos[d] + node.size[d],
            ) {
                self.hovered.push(n);
            }

            if node.style.bg_color != 0 {
                buf.push(DrawCmd {
                    elem: n,
                    data: DrawCmdData::Rect(RectCmdData {
                        x: node.pos.x,
                        y: node.pos.y,
                        width: node.size.x,
                        height: node.size.y,
                        color: node.style.bg_color,
                    }),
                });
            }

            if let Some((font, _)) = get_text_params(ctx, n) {
                let [_, y] = measure_text!(ctx, n, 0.);

                measure_text_ext(
                    ctx,
                    backend,
                    n,
                    0.,
                    &mut |content, y_off, width| {
                        // TODO: check bounds and skip this if possible
                        buf.push(DrawCmd {
                            elem: n,
                            data: DrawCmdData::Text(TextCmdData {
                                x: node.pos.x
                                    + node.style.padding.x.before
                                    + node.style.align.x.offset(
                                        node.inner_size(Dim::X) - width,
                                    ),
                                y: node.pos.y
                                    + node.style.padding.y.before
                                    + node
                                        .style
                                        .align
                                        .y
                                        .offset(node.inner_size(Dim::Y) - y)
                                    + y_off,
                                size: node.style.font_size,
                                spacing: node.style.font_spacing,
                                line_spacing: node.style.font_line_spacing,
                                content,
                                font,
                                color: node.style.fg_color,
                            }),
                        });
                    },
                );
            }
        }

        buf.leak()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeasureCacheKey {
    pub text_hash: u32,
    pub elem_id: ElemID,
    pub max_width: f32,
}

impl MeasureCacheKey {
    pub fn hash(&self) -> u8 {
        (fnv1a(unsafe {
            slice::from_raw_parts(
                self as *const _ as *const u8,
                core::mem::size_of::<Self>(),
            )
        }) as u8)
            .max(1)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MeasureCacheEntry {
    pub key: MeasureCacheKey,
    pub dims: [f32; DIMS],
    pub priority: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct MeasureCache {
    hashes: [u8; Self::CAP],
    entries: [MeasureCacheEntry; Self::CAP],
    hits: u64,
    len: usize,
}

impl MeasureCache {
    const CAP: usize = 64;
    const MIN_LEN_TO_CACHE: u32 = 32;

    pub fn prune(&mut self) {
        assert!(self.len < self.entries.len());

        // TODO: we could do better but eh
        let mut keep = 0;
        for i in 0..self.len {
            if self.hits & 1 << i != 0 {
                self.entries[keep] = self.entries[i];
                self.hashes[keep] = self.hashes[i];
                keep += 1;
            }
        }
        self.len = keep;

        self.hashes[self.len..].fill(0);

        self.hits = 0;
    }

    pub fn get_or_insert(
        &mut self,
        len: u32,
        key: MeasureCacheKey,
        measure: &mut dyn FnMut() -> [f32; DIMS],
    ) -> [f32; DIMS] {
        assert!(self.len < self.entries.len());

        if len < Self::MIN_LEN_TO_CACHE {
            return measure();
        }

        let hash = key.hash();

        let idx;

        match SimdIter::new_min_aligned(&self.hashes[..], self.len, hash)
            .find(|&i| self.entries[i].key == key)
        {
            Some(i) => idx = i,
            None => {
                let dims = measure();

                idx = if self.len == self.entries.len() {
                    let (min_prio, min_idx) = self
                        .entries
                        .iter()
                        .enumerate()
                        .map(|(i, e)| (e.priority, i))
                        .min()
                        .expect("we are full, so there needs to be somethin");

                    if min_prio > len {
                        return dims;
                    }

                    min_idx
                } else {
                    self.len += 1;
                    self.len - 1
                };

                self.hashes[idx] = hash;
                self.entries[idx] =
                    MeasureCacheEntry { key, dims, priority: len };
            }
        };

        self.hits |= 1 << idx;

        self.entries[idx].dims
    }
}

impl Default for MeasureCache {
    fn default() -> Self {
        Self {
            hashes: [Default::default(); Self::CAP],
            entries: [Default::default(); Self::CAP],
            len: Default::default(),
            hits: Default::default(),
        }
    }
}

#[derive(Debug)]
pub struct FrameCtx {
    elemets: Vec<Elem>,
    text: TextBuf,
}

impl Default for FrameCtx {
    fn default() -> Self {
        Self {
            elemets: alloc::vec![Default::default()],
            text: Default::default(),
        }
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
                elem.style.size.x,
                elem.style.size.y,
                elem.pos.x,
                elem.pos.y,
                elem.size.x,
                elem.size.y,
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
        ElemIdx((self.elemets.len() - 1) as u32)
    }
}

/// .0 == 0 is reserved for anonimous elements
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct ElemID(pub u32);

impl ElemID {
    pub fn hash(self) -> u8 {
        (self.0 as u8).max(1)
    }

    pub const fn idx(self, value: usize) -> Self {
        Self(mix_u32(self.0, value as u32))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct ElemIdx(pub u32);

#[repr(transparent)]
pub struct AnonElemBuilder<'a>(pub ElemBuilder<'a>);

impl<'a> AnonElemBuilder<'a> {
    pub fn style(self, modifier: impl FnOnce(Style) -> Style) -> Self {
        Self(self.0.style(|_, s| modifier(s)))
    }
}

pub struct ElemBuilder<'b> {
    pub ctx: &'b Ctx,
    pub idx: ElemIdx,
    pub prev_id: Cell<ElemIdx>,
}

impl<'b> ElemBuilder<'b> {
    pub fn style(self, modifier: impl FnOnce(&Self, Style) -> Style) -> Self {
        let mut style = self.ctx.elem(0, self.idx).style;
        style = modifier(&self, style);
        self.ctx.elem_mut(0, self.idx).style = style;

        self
    }

    pub fn id(&self) -> ElemID {
        let id = self.ctx.elem(0, self.idx).id;
        debug_assert!(
            id.0 != 0,
            "this operation is not supported on anonimous\
            elements, use .el(id!()) at least (performance reasons)"
        );
        id
    }

    pub fn prev(&self) -> Ref<'b, Elem> {
        let id = self.id();
        if self.prev_id.get().0 == 0 {
            self.prev_id.set(self.ctx.elem_idx_by_id(id));
        }
        self.ctx.elem(1, self.prev_id.get())
    }

    pub fn hovered(&self) -> bool {
        self.ctx
            .hovered
            .last()
            .map_or(false, |&n| self.ctx.elem(1, n).id == self.id())
    }

    /// This is ture for any element that overlaps with mouse
    pub fn any_hovered(&self) -> bool {
        let id = self.id();
        self.ctx.hovered.iter().any(|&n| self.ctx.elem(1, n).id == id)
    }

    pub fn scope<T>(self, content: impl FnOnce(Self) -> T) -> T {
        content(self)
    }
}

impl Drop for ElemBuilder<'_> {
    fn drop(&mut self) {
        self.ctx.parent.set(self.ctx.elem_mut(0, self.idx).parent);
        let mut parent = self.ctx.elem_mut(0, self.ctx.parent.get());
        if parent.first_child.0 == 0 {
            parent.first_child = self.idx;
        }
        self.ctx.current_elem.set(self.idx);
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Elem {
    pub id: ElemID,
    parent: ElemIdx,
    first_child: ElemIdx,
    next: ElemIdx,

    pub style: Style,

    pub pos: Dims<f32>,
    pub size: Dims<f32>,
}

impl Elem {
    pub fn inner_size(&self, dim: Dim) -> f32 {
        self.size[dim] - self.style.padding[dim].sum()
    }

    pub fn outer_size(&self, dim: Dim) -> f32 {
        self.size[dim] + self.style.margin[dim].sum()
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

#[derive(Clone, Copy, Default, Debug)]
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

pub struct StyleScope<'b> {
    pub ctx: &'b Ctx,
    pub prev: Style,
}

impl Drop for StyleScope<'_> {
    fn drop(&mut self) {
        self.ctx.style.set(self.prev);
    }
}

#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dim {
    X,
    Y,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, PartialOrd)]
pub struct Bounds {
    before: f32,
    after: f32,
}

impl Bounds {
    pub fn sum(self) -> f32 {
        self.before + self.after
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dims<T> {
    pub x: T,
    pub y: T,
}

impl<T> Index<Dim> for Dims<T> {
    type Output = T;

    fn index(&self, index: Dim) -> &Self::Output {
        match index {
            Dim::X => &self.x,
            Dim::Y => &self.y,
        }
    }
}

impl<T> IndexMut<Dim> for Dims<T> {
    fn index_mut(&mut self, index: Dim) -> &mut Self::Output {
        match index {
            Dim::X => &mut self.x,
            Dim::Y => &mut self.y,
        }
    }
}

pub trait TrueDims {
    type Elem;
}

impl<T> TrueDims for Dims<T> {
    type Elem = T;
}

macro_rules! derive_style_builder {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[set($dim_field_mod_x:ident, $dim_field_mod_y:ident)])?
                $(#[conv($field_mod:ident)])?
                $(#[custom($custom_marker:tt)])?
                $field_vis:vis $field_name:ident: $field_type:ty,
            )*
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $(
                $field_vis $field_name: $field_type,
            )*
        }

        $(
            derive_style_builder!(@$(custom($custom_marker))?
                impl $name {
                        $field_vis fn $field_name(mut self, value:
                            derive_style_builder!(@$($field_mod)? $field_type)) -> Self {
                            self.$field_name = value.into();
                            self
                        }
                }
            );
        )*

        $(
            derive_style_builder!(@dim_set x $(name $dim_field_mod_x)? $name,
                $field_vis, $field_name, $field_type);
            derive_style_builder!(@dim_set y $(name $dim_field_mod_y)? $name,
                $field_vis, $field_name, $field_type);
        )*
    };

    (@into $field_type:ty) => {impl Into<$field_type>};
    (@dim_set $idx:ident name $name:ident $typename:ident, $vis:vis,
        $field_name:ident, $dim_field_type:ty) => {
        impl $typename {
            $vis fn $name(mut self, value:
               <$dim_field_type as TrueDims>::Elem) -> Self {
                self.$field_name.$idx = value;
                self
            }
        }
    };
    (@dim_set $idx:ident $typename:ident, $vis:vis,
        $field_name:ident, $dim_field_type:ty) => {};
    (@custom(_) $($tt:tt)*) => {};
    (@ $($tt:tt)*) => {$($tt)*};
}

derive_style_builder! {
    #[derive(Clone, Copy, Default, Debug)]
    pub struct Style {
        #[set(align_x, align_y)]
        pub align: Dims<Align>,
        #[set(width, height)]
        pub size: Dims<f32>,
        #[set(offset_x, offset_y)]
        pub offset: Dims<f32>,
        #[set(min_width, min_height)]
        pub min_size: Dims<f32>,
        #[custom(_)]
        pub padding: Dims<Bounds>,
        #[custom(_)]
        pub margin: Dims<Bounds>,
        #[set(scroll_x, scroll_y)]
        pub scroll: Dims<f32>,
        pub bg_color: Color,
        pub fg_color: Color,
        pub layout: Layout,
        pub direction: Direction,
        #[conv(into)]
        pub text: TextId,
        pub self_align: Align,
        pub gap: f32,
        #[conv(into)]
        pub font: Option<FontId>,
        pub font_size: f32,
        pub font_spacing: f32,
        pub font_line_spacing: f32,
        pub layer: u32,
    }
}

impl Style {
    /// you can pass [padding_left, padding_top, padding_bottom, padding_right],
    /// [padding_x, padding_y], [padding]
    pub fn padding<const SIZE: usize>(mut self, values: [f32; SIZE]) -> Self {
        self.padding.x.before = values[0 % SIZE];
        self.padding.x.after = values[2 % SIZE];
        self.padding.y.before = values[1 % SIZE];
        self.padding.y.after = values[3 % SIZE];
        self
    }

    /// you can pass [margin_left, margin_top, margin_bottom, margin_right],
    /// [margin_x, margin_y], [margin]
    pub fn margin<const SIZE: usize>(mut self, values: [f32; SIZE]) -> Self {
        self.margin.x.before = values[0 % SIZE];
        self.margin.x.after = values[2 % SIZE];
        self.margin.y.before = values[1 % SIZE];
        self.margin.y.after = values[3 % SIZE];
        self
    }

    pub fn width_fit(mut self) -> Self {
        self.size.x = 0.;
        self
    }

    pub fn height_fit(mut self) -> Self {
        self.size.y = 0.;
        self
    }

    pub fn width_perc(mut self, value: f32) -> Self {
        self.size.x = -value;
        self
    }

    pub fn height_perc(mut self, value: f32) -> Self {
        self.size.y = -value;
        self
    }

    pub fn width_grow(mut self) -> Self {
        self.size.x = -1.;
        self
    }

    pub fn height_grow(mut self) -> Self {
        self.size.y = -1.;
        self
    }
}

pub type Color = u32;

pub const RED: Color = 0xfb532bff;
pub const GREEN: Color = 0x91fb2bff;
pub const BLUE: Color = 0x1064ffff;
pub const WHITE: Color = 0xebebebff;

pub const fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let mut out = 0;

    let mut i = 0;
    while i < 4 {
        let shift = i * 8;
        let a = ((a >> shift) & 0xff) as f32;
        let b = ((b >> shift) & 0xff) as f32;
        let v = lerp(a, b, t);

        out |= (v as u32) << shift;
        i += 1;
    }

    out
}

pub const fn lerp(a: f32, b: f32, t: f32) -> f32 {
    if (a - b).abs() <= 1. {
        return b;
    }

    a + (b - a) * t
}

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

#[derive(Clone, Copy, Default, Debug)]
pub enum Layout {
    #[default]
    Flex,
}

#[derive(Clone, Copy, Default, Debug)]
pub enum Direction {
    #[default]
    Left2Right,
    Top2Bottom,
}

impl Direction {
    pub fn dims(self) -> [Dim; DIMS] {
        match self {
            Direction::Left2Right => [Dim::X, Dim::Y],
            Direction::Top2Bottom => [Dim::Y, Dim::X],
        }
    }
}
