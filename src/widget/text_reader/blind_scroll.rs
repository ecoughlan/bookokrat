use super::*;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlindPage {
    chapter: usize,
    start: usize,
    rows: usize,
}

pub(super) struct BlindScroll {
    origin: usize,
    chapter: usize,
    tail_chapter: usize,
    pub(super) chapters: HashMap<usize, Box<MarkdownTextReader>>,
    pages: VecDeque<BlindPage>,
    first_page: usize,
    page: usize,
    height: usize,
    columns: usize,
    row: usize,
    /// Words and cell width of the row being revealed; they set its duration and wipe length.
    words: usize,
    width: usize,
    eof: bool,
    ready: bool,
    paused: bool,
    last_tick: Instant,
    elapsed: Duration,
}

impl BlindScroll {
    fn new(origin: usize, chapter: usize, now: Instant) -> Self {
        Self {
            origin,
            chapter,
            tail_chapter: chapter,
            chapters: HashMap::new(),
            pages: VecDeque::new(),
            first_page: 0,
            page: 0,
            height: 0,
            columns: 1,
            row: 0,
            words: 0,
            width: 0,
            eof: false,
            ready: false,
            paused: false,
            last_tick: now,
            elapsed: Duration::ZERO,
        }
    }

    fn page_at(&self, index: usize) -> Option<BlindPage> {
        index
            .checked_sub(self.first_page)
            .and_then(|index| self.pages.get(index))
            .copied()
    }

    fn spread(&self) -> usize {
        self.page / self.columns * self.columns
    }

    fn needs_chapter(&self) -> bool {
        self.height > 0 && !self.eof && self.page_at(self.spread() + self.columns * 2 - 1).is_none()
    }

    fn append_chapter(&mut self, chapter: usize, start: usize, total: usize) {
        self.tail_chapter = chapter;
        self.pages
            .extend((start..total).step_by(self.height).map(|start| BlindPage {
                chapter,
                start,
                rows: (total - start).min(self.height),
            }));
    }

    fn unread_page(&self) -> Option<BlindPage> {
        self.page_at(self.page)
            .or_else(|| self.pages.back().copied())
    }

    fn unread_position(&self) -> Option<(usize, usize)> {
        if let Some(page) = self.page_at(self.page) {
            Some((page.chapter, page.start + self.row.min(page.rows - 1)))
        } else {
            self.pages
                .back()
                .map(|page| (page.chapter, page.start + page.rows - 1))
        }
    }

    fn sweep_height(&self, index: usize) -> usize {
        self.page_at(index).map_or(0, |page| page.rows).max(
            self.page_at(index + self.columns)
                .map_or(0, |page| page.rows),
        )
    }

    fn step(&mut self, rows: isize, within_chapter: bool) -> bool {
        if self.height == 0 || self.needs_chapter() {
            return false;
        }
        let chapter = self.page_at(self.page).map(|page| page.chapter);
        let mut changed = false;
        for _ in 0..rows.unsigned_abs() {
            let (page, row) = if rows > 0 {
                if self.page_at(self.page).is_none() {
                    break;
                }
                if self.row + 1 < self.sweep_height(self.page) {
                    (self.page, self.row + 1)
                } else {
                    (self.page + 1, 0)
                }
            } else if self.row > 0 {
                (self.page, self.row - 1)
            } else if self.page > self.first_page {
                let previous = self.page - 1;
                (previous, self.sweep_height(previous).saturating_sub(1))
            } else {
                break;
            };
            if within_chapter && self.page_at(page).map(|page| page.chapter) != chapter {
                break;
            }
            if !self.eof && self.page_at(page).is_none() {
                break;
            }
            self.page = page;
            self.row = row;
            changed = true;
            if self.needs_chapter() {
                break;
            }
        }
        changed
    }

    /// Reading time of the row being revealed; rows without words are free.
    fn interval(&self, speed: u16) -> Duration {
        Duration::from_secs_f64(60.0 * self.words as f64 / f64::from(speed.clamp(50, 1000)))
    }

    /// Columns of the row being revealed that are already showing the next page.
    fn revealed(&self, speed: u16) -> usize {
        let progress = self.elapsed.as_secs_f64() / self.interval(speed).as_secs_f64();
        ((progress * self.width as f64) as usize).min(self.width)
    }

    fn advance(&mut self, now: Instant, speed: u16) -> bool {
        let delta = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        if self.needs_chapter() {
            self.ready = false;
            self.elapsed = Duration::ZERO;
            return false;
        }
        if !self.ready {
            self.ready = self.height > 0;
            return false;
        }
        if self.paused || self.page_at(self.page).is_none() {
            return false;
        }
        let interval = self.interval(speed);
        let revealed = self.revealed(speed);
        self.elapsed += delta.min(interval);
        let stepped = if self.elapsed < interval {
            false
        } else {
            self.elapsed -= interval;
            self.step(1, false)
        };
        stepped || self.revealed(speed) != revealed
    }

    fn parts(&self, column: usize) -> [(Option<BlindPage>, usize, usize); 2] {
        let active = self.page % self.columns;
        let split = if column < active {
            self.height
        } else if column == active {
            self.row
        } else {
            0
        };
        let old = self.page_at(self.spread() + column);
        let next = self.page_at(self.spread() + self.columns + column).or(old);
        [(next, 0, split), (old, split, self.height)]
    }

    fn prune(&mut self) {
        let Some(oldest) = self
            .page_at(self.spread())
            .or_else(|| self.pages.back().copied())
        else {
            return;
        };
        while self
            .pages
            .front()
            .is_some_and(|page| page.chapter < oldest.chapter)
        {
            self.pages.pop_front();
            self.first_page += 1;
        }
        self.chapters
            .retain(|chapter, _| *chapter >= oldest.chapter);
    }
}

impl MarkdownTextReader {
    pub fn start_blind_scroll(&mut self, chapter: usize) -> bool {
        if self.markdown_document.is_none()
            || self.show_raw_html
            || self.normal_mode.is_active()
            || self.comment_input.is_active()
            || self.search_state.mode == SearchMode::InputMode
        {
            return false;
        }
        self.clear_selection();
        self.auto_scroll_active = false;
        self.highlight_visual_line = None;
        self.blind_scroll = Some(BlindScroll::new(
            self.reading_line(),
            chapter,
            Instant::now(),
        ));
        self.blind_scroll_resume_line = None;
        self.last_overlay_cleanup_key = None;
        true
    }

    fn blind_line_count(&self) -> usize {
        self.rendered_content
            .lines
            .iter()
            .rposition(|line| {
                line.node_index.is_some()
                    || !line.raw_text.trim().is_empty()
                    || matches!(line.line_type, LineType::ImagePlaceholder { .. })
            })
            .map_or(0, |line| line + 1)
    }

    pub fn blind_scroll_needed_chapter(&self) -> Option<usize> {
        self.blind_scroll
            .as_ref()
            .filter(|state| state.needs_chapter())
            .map(|state| state.tail_chapter + 1)
    }

    pub fn blind_scroll_chapter(&self) -> Option<usize> {
        self.blind_scroll.as_ref().map(|state| state.chapter)
    }

    pub fn finish_blind_scroll_loading(&mut self) {
        if let Some(state) = self.blind_scroll.as_mut() {
            state.eof = true;
        }
    }

    pub fn cache_blind_scroll_chapter(
        &mut self,
        chapter: usize,
        href: String,
        html: &str,
        title: Option<String>,
        images: &crate::images::book_images::BookImages,
    ) {
        let mut reader = Self::with_image_picker(self.image_picker.clone(), self.settings.clone());
        reader.content_margin = self.content_margin;
        reader.vertical_margin = self.vertical_margin;
        reader.justify_text = self.justify_text;
        reader.underline_color_enabled = self.underline_color_enabled;
        reader.dual.enabled = self.dual.enabled;
        reader.book_comments = self.book_comments.clone();
        reader.set_current_chapter_file(Some(href));
        reader.set_content_from_string(html, title);
        reader.image_viewport = self.image_viewport;
        reader.preload_image_dimensions(images);
        reader.prepare_content(
            self.last_width,
            &crate::theme::current_theme(),
            self.last_focus_state,
        );
        let total = reader.blind_line_count();
        if let Some(state) = self.blind_scroll.as_mut() {
            state.append_chapter(chapter, 0, total);
            if total > 0 {
                state.chapters.insert(chapter, Box::new(reader));
            }
        }
    }

    fn measure_blind_row(&mut self, state: &mut BlindScroll) {
        let [(next, ..), (_, row, _)] = state.parts(state.page % state.columns);
        (state.words, state.width) = next
            .and_then(|page| {
                let owner = self.blind_owner(state, page.chapter)?;
                let text = &owner.rendered_content.lines.get(page.start + row)?.raw_text;
                Some((text.split_whitespace().count(), text.chars().count()))
            })
            .unwrap_or_default();
    }

    fn sync_blind_scroll_position(&mut self, state: &mut BlindScroll) {
        if let Some((chapter, line)) = state.unread_position() {
            if chapter != state.chapter {
                if let Some(mut next) = state.chapters.remove(&chapter) {
                    next.blind_scroll_speed = self.blind_scroll_speed;
                    next.last_content_area = self.last_content_area;
                    next.visible_height = self.visible_height;
                    next.last_rendered_image_rects =
                        std::mem::take(&mut self.last_rendered_image_rects);
                    next.inline_images_suppressed = self.inline_images_suppressed;
                    next.dual.active = self.dual.active;
                    let previous = std::mem::replace(self, *next);
                    state.chapters.insert(state.chapter, Box::new(previous));
                    state.chapter = chapter;
                }
            }
            self.scroll_offset = line;
        }
        state.prune();
        self.measure_blind_row(state);
        self.last_overlay_cleanup_key = None;
    }

    /// Where blind scrolling stopped, as long as that line is still on screen.
    pub(super) fn resume_line(&self) -> Option<usize> {
        self.blind_scroll_resume_line.filter(|&line| {
            (self.scroll_offset..self.scroll_offset + self.visible_height).contains(&line)
        })
    }

    pub(super) fn reading_line(&self) -> usize {
        self.blind_scroll
            .as_ref()
            .and_then(|state| state.unread_position().map(|(_, line)| line))
            .or_else(|| self.resume_line())
            .unwrap_or(self.scroll_offset)
    }

    pub fn is_blind_scrolling(&self) -> bool {
        self.blind_scroll.is_some()
    }

    pub fn is_blind_scroll_paused(&self) -> bool {
        self.blind_scroll.as_ref().is_some_and(|state| state.paused)
    }

    pub fn is_blind_scroll_finished(&self) -> bool {
        self.blind_scroll.as_ref().is_some_and(|state| {
            state.height > 0 && state.eof && state.page_at(state.page).is_none()
        })
    }

    pub fn stop_blind_scroll(&mut self) -> bool {
        let Some(mut state) = self.blind_scroll.take() else {
            return false;
        };
        self.sync_blind_scroll_position(&mut state);
        let unread = self.scroll_offset;
        // The page stays where it was; only the first unrevealed row is flashed.
        let top = state.unread_page().map_or(unread, |page| page.start);
        if self.dual.active && state.height > 0 {
            self.sync_dual_geometry(state.height);
            self.dual_scroll_to_line(top);
        } else {
            self.scroll_offset = top;
        }
        self.blind_scroll_resume_line = Some(unread);
        self.highlight_line_temporarily(unread, Duration::from_secs(2));
        self.last_overlay_cleanup_key = None;
        true
    }

    pub fn pause_blind_scroll(&mut self) {
        if let Some(state) = self.blind_scroll.as_mut() {
            state.paused = !state.paused;
            state.last_tick = Instant::now();
        }
    }

    pub fn get_blind_scroll_speed(&self) -> u16 {
        self.blind_scroll_speed
    }

    pub fn set_blind_scroll_speed(&mut self, speed: u16) {
        self.blind_scroll_speed = speed.clamp(50, 1000);
        if let Some(state) = self.blind_scroll.as_mut() {
            state.elapsed = Duration::ZERO;
            state.last_tick = Instant::now();
        }
    }

    pub fn step_blind_scroll(&mut self, rows: isize) -> bool {
        let Some(mut state) = self.blind_scroll.take() else {
            return false;
        };
        let changed = state.step(rows, true);
        state.elapsed = Duration::ZERO;
        state.last_tick = Instant::now();
        self.sync_blind_scroll_position(&mut state);
        self.blind_scroll = Some(state);
        changed
    }

    pub fn update_blind_scroll(&mut self, now: Instant) -> bool {
        let Some(mut state) = self.blind_scroll.take() else {
            return false;
        };
        let chapter = state.chapter;
        let mut changed = false;
        // A run of wordless rows is revealed in one tick.
        loop {
            let stepped = state.advance(now, self.blind_scroll_speed);
            self.sync_blind_scroll_position(&mut state);
            changed |= stepped;
            if !stepped || state.words > 0 {
                break;
            }
        }
        let changed = changed || chapter != state.chapter;
        self.blind_scroll = Some(state);
        changed
    }

    pub(super) fn blind_scroll_title(&self) -> Option<Line<'static>> {
        self.blind_scroll.as_ref().map(|state| {
            let mode = if state.paused { "PAUSED" } else { "BLIND" };
            Line::from(format!(
                " {mode} {} wpm | Space: pause | ↑↓: reveal (Shift: 10) | +/-: speed | Esc: stop ",
                self.blind_scroll_speed
            ))
        })
    }

    fn blind_owner<'a>(
        &'a mut self,
        state: &'a mut BlindScroll,
        chapter: usize,
    ) -> Option<&'a mut MarkdownTextReader> {
        if chapter == state.chapter {
            Some(self)
        } else {
            state.chapters.get_mut(&chapter).map(Box::as_mut)
        }
    }

    pub(super) fn render_blind_scroll(
        &mut self,
        frame: &mut Frame,
        left: Rect,
        right: Option<Rect>,
        palette: &Base16Palette,
        selection_bg: RatatuiColor,
        suppress_images: bool,
    ) {
        let Some(mut state) = self.blind_scroll.take() else {
            return;
        };
        if left.height == 0 {
            self.blind_scroll = Some(state);
            return;
        }
        if state.height == 0 {
            state.height = usize::from(left.height);
            state.columns = if right.is_some() { 2 } else { 1 };
            if right.is_some() {
                state.origin = state.origin / (state.height * 2) * state.height * 2;
            }
            state.origin = state.origin.min(self.blind_line_count().saturating_sub(1));
            state.append_chapter(state.chapter, state.origin, self.blind_line_count());
            state.ready = !state.needs_chapter();
            state.last_tick = Instant::now();
            self.scroll_offset = state.origin;
            self.measure_blind_row(&mut state);
        }
        let revealed = state.revealed(self.blind_scroll_speed);
        let mut image_rects: HashMap<String, Rect> = HashMap::new();
        for (column, rect) in [Some(left), right].into_iter().flatten().enumerate() {
            let parts = state.parts(column);
            for (page, from, to) in parts {
                let Some(page) = page else {
                    continue;
                };
                let to = to.min(page.rows);
                if from >= to {
                    continue;
                }
                let Some(owner) = self.blind_owner(&mut state, page.chapter) else {
                    continue;
                };
                let start = page.start + from;
                let part = Rect::new(rect.x, rect.y + from as u16, rect.width, (to - from) as u16);
                let lines: Vec<_> = (start..start + to - from)
                    .map(|line| owner.styled_line(line, palette, selection_bg))
                    .collect();
                frame.render_widget(Paragraph::new(lines), part);
                if !suppress_images {
                    let mut part_images = HashMap::new();
                    owner.render_column_images(frame, start, part, None, 0, &mut part_images);
                    for (src, image_rect) in part_images {
                        image_rects
                            .entry(format!("{}:{src}", page.chapter))
                            .and_modify(|rect| *rect = rect.union(image_rect))
                            .or_insert(image_rect);
                    }
                }
            }
            if column != state.page % state.columns {
                continue;
            }
            let row = rect.y + state.row as u16;
            let (y, end) = if revealed > 0 {
                // The row being revealed shows the next page left of the wipe and
                // the old page right of it; images stay with the old row until it
                // is fully replaced.
                let [(next, ..), (_, split, _)] = parts;
                let mut line = next
                    .and_then(|page| {
                        self.blind_owner(&mut state, page.chapter).map(|owner| {
                            owner.styled_line(page.start + split, palette, selection_bg)
                        })
                    })
                    .unwrap_or_default();
                line.spans.push(Span::raw(" ".repeat(revealed)));
                frame.render_widget(
                    Paragraph::new(line),
                    Rect::new(rect.x, row, revealed as u16, 1),
                );
                (row, rect.x + revealed as u16)
            } else {
                (row.saturating_sub(1), rect.right().saturating_sub(2))
            };
            for x in rect.x..end.min(rect.right()) {
                if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                    cell.set_style(RatatuiStyle::default().add_modifier(Modifier::UNDERLINED));
                }
            }
        }
        self.last_rendered_image_rects = image_rects;
        self.blind_scroll = Some(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(height: usize, columns: usize, total: usize) -> BlindScroll {
        let mut state = BlindScroll::new(0, 0, Instant::now());
        state.height = height;
        state.words = 1;
        state.columns = columns;
        state.append_chapter(0, 0, total);
        state.eof = true;
        state.ready = true;
        state
    }

    fn rows(state: &BlindScroll, column: usize) -> Vec<(usize, usize)> {
        state
            .parts(column)
            .into_iter()
            .flat_map(|(page, from, to)| {
                page.into_iter().flat_map(move |page| {
                    (from..to.min(page.rows)).map(move |row| (page.chapter, page.start + row))
                })
            })
            .collect()
    }

    #[test]
    fn blind_scroll_wipes_left_then_right_without_moving_unread_rows() {
        let mut state = state(3, 2, 20);
        for step in 0..=6 {
            assert_eq!(state.unread_position(), Some((0, step)));
            assert_eq!(
                rows(&state, 0),
                (0..3)
                    .map(|row| (0, if row < step { row + 6 } else { row }))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                rows(&state, 1),
                (3..6)
                    .map(|row| (0, if row < step { row + 6 } else { row }))
                    .collect::<Vec<_>>()
            );
            assert!(state.step(1, false));
        }
    }

    #[test]
    fn blind_scroll_seamless_short_chapters_and_reverse_boundary() {
        let mut state = state(3, 2, 3);
        for chapter in 1..=3 {
            state.append_chapter(chapter, 0, 3);
        }
        assert_eq!(rows(&state, 1), vec![(1, 0), (1, 1), (1, 2)]);
        assert!(state.step(1, false));
        assert_eq!(rows(&state, 0), vec![(2, 0), (0, 1), (0, 2)]);
        assert!(state.step(2, false));
        assert_eq!(state.unread_position(), Some((1, 0)));
        assert!(!state.step(-1, true));
        assert!(state.step(1, true));
        assert_eq!(rows(&state, 1), vec![(3, 0), (1, 1), (1, 2)]);
    }

    #[test]
    fn blind_scroll_steps_are_reversible_across_spreads() {
        let mut state = state(3, 2, 100);
        let before = [rows(&state, 0), rows(&state, 1)];
        assert!(state.step(10, true));
        assert_eq!(state.unread_position(), Some((0, 10)));
        assert!(state.step(-10, true));
        assert_eq!([rows(&state, 0), rows(&state, 1)], before);
        assert!(!state.step(-1, true));
    }

    #[test]
    fn blind_scroll_pause_and_stalls_never_skip_unread_lines() {
        let mut state = state(10, 1, 100);
        let start = state.last_tick;
        assert!(!state.advance(start + Duration::from_millis(500), 60));
        state.paused = true;
        assert!(!state.advance(start + Duration::from_secs(60), 60));
        state.paused = false;
        assert!(state.advance(start + Duration::from_millis(60500), 60));
        assert_eq!(state.unread_position(), Some((0, 1)));
        assert!(state.advance(start + Duration::from_secs(600), 60));
        assert_eq!(state.unread_position(), Some((0, 2)));
    }
}
