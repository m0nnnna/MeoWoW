//! The auction window: fifty rows out of however many there are.
//!
//! Every other list frame in this interface draws the whole of its subject. A
//! trainer's spells, a mailbox's letters, a guild's members, a corpse's loot
//! -- the window is as tall as the thing it is about, and when it runs out of
//! rows there are no more. This one is not, and pretending otherwise is the
//! single thing that would make it wrong.
//!
//! ## The window has to say that it is a window
//!
//! [`AuctionView::total`] and the number of rows are drawn as **one sentence**
//! -- "51-100 of 1,284" -- rather than as a row count with a total tucked
//! somewhere else, because the failure mode is a person reading fifty rows and
//! believing they are the market. That is not a rendering bug. The rows are
//! all real, the prices are all real, and the decision made on them is wrong.
//! There is no visual difference between the honest version and the dishonest
//! one except this line, which is why it is drawn even when the match fits in
//! one page and even when it is empty.
//!
//! The mailbox window has the same shape and a weaker version of the problem
//! -- `MailView::withheld` exists for exactly this -- but a full mailbox is
//! fifty letters and an auction house is tens of thousands. Here the surplus
//! is the normal case rather than the exception.
//!
//! ## Sorting is not offered, because this window cannot do it
//!
//! There are no clickable column headers. Sorting fifty rows out of 1,284 by
//! price produces the cheapest of *these fifty*, in price order, looking
//! exactly like the cheapest fifty in the house. The server sorts -- the sort
//! order travels in the request -- so a sort control belongs to the search and
//! not to the table, and a table that sorted itself would be a control that
//! silently answers a different question from the one it appears to answer.
//!
//! Until the search side carries it, the honest thing is to have no control at
//! all. See `world::auction`.
//!
//! ## Time is a band and not a clock
//!
//! The wire carries milliseconds remaining, captured when the packet was
//! built. A window open for two minutes is counting down from a number that
//! was already two minutes stale, so a clock would be precise and wrong.
//! Four bands is what the original client showed and what the number can
//! honestly support.
//!
//! ## What a click does, and what it deliberately does not
//!
//! A click selects. Bidding and buying out are **separate buttons under the
//! list**, not the row gesture, because they spend money and cannot be undone
//! -- the same caution that keeps deleting a letter off the row gesture in the
//! mailbox. The buttons name their prices, so the number a person is agreeing
//! to is on screen before they press anything, and the window says what the
//! gesture is in a line under the list. That last part is 4.26's lesson
//! applied before the first live test rather than after it.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Which of the three lists is on screen.
///
/// They are the same packet layout and three different questions, and the tab
/// is the only thing that says which -- a browse result and an owner list are
/// byte-identical in shape and mean opposite things about who owns the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuctionTab {
    /// A search of the whole house. **The only one that pages.**
    #[default]
    Browse,
    /// What this character is bidding on.
    Bids,
    /// What this character is selling.
    Selling,
}

impl AuctionTab {
    pub const ALL: [AuctionTab; 3] = [AuctionTab::Browse, AuctionTab::Bids, AuctionTab::Selling];

    pub fn label(self) -> &'static str {
        match self {
            AuctionTab::Browse => "Browse",
            AuctionTab::Bids => "Bids",
            AuctionTab::Selling => "Auctions",
        }
    }

    /// Whether this list can have more rows than arrived.
    ///
    /// Read by the drawing so the count line can say "of 1,284" on a browse
    /// and simply "12 auctions" on the other two, rather than implying a
    /// surplus that cannot exist.
    pub fn pages(self) -> bool {
        matches!(self, AuctionTab::Browse)
    }
}

/// How much time is left, to the precision the wire can honestly support.
///
/// Mirrors `world::auction::TimeBand` rather than borrowing it, because this
/// crate depends on neither `world` nor `render` -- which is what keeps the
/// whole interface testable with no connection and no GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeBand {
    Short,
    Medium,
    Long,
    VeryLong,
}

impl TimeBand {
    pub fn label(self) -> &'static str {
        match self {
            TimeBand::Short => "Short",
            TimeBand::Medium => "Medium",
            TimeBand::Long => "Long",
            TimeBand::VeryLong => "Very Long",
        }
    }
}

/// One auction on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct AuctionRow {
    /// **The server's auction id**, and what a bid or a cancellation names.
    /// Never a row position -- and here the reason is sharper than usual,
    /// because row three of page two and row three of page one are different
    /// auctions that would both be "3".
    pub id: u32,
    /// Already resolved from the item entry by the caller, because the list
    /// result carries no names at all.
    pub name: String,
    /// Stack size. A stack is bought whole, so this multiplies the price a
    /// person is actually agreeing to and is drawn beside the name rather
    /// than in a column somebody might not read.
    pub count: u32,
    pub icon: Option<egui::TextureId>,
    pub seller: String,
    /// The current bid, or zero when nobody has bid.
    pub bid: u32,
    /// What a bid must be, already worked out from the two different fields
    /// it comes from. See `world::auction::Auction::next_bid`.
    pub next_bid: u32,
    /// Zero means the seller offered no buyout, and the column is left blank
    /// rather than showing a zero price.
    pub buyout: u32,
    pub band: TimeBand,
    /// Whether this character is the seller. The server refuses a bid on your
    /// own auction, so the row is dimmed and inert.
    pub own: bool,
}

impl AuctionRow {
    /// Whether clicking this row should select it.
    ///
    /// Read by the drawing **and** the hit test, from here, so the two cannot
    /// drift into a row that looks selectable and is not -- the rule the
    /// trainer and mailbox windows both made load-bearing.
    ///
    /// Your own auctions are inert on the browse and bid tabs. They are not
    /// inert on the selling tab, which is the tab where the only action *is*
    /// on your own rows -- so this takes the tab rather than answering from
    /// the row alone.
    pub fn selectable(&self, tab: AuctionTab) -> bool {
        match tab {
            AuctionTab::Selling => self.own,
            _ => !self.own,
        }
    }
}

/// Everything the auction window draws.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AuctionView {
    pub tab: AuctionTab,
    /// The rows in the page that arrived. At most fifty on a browse.
    pub rows: Vec<AuctionRow>,
    /// **How many matched in the whole house.** The field this window exists
    /// to be honest about.
    pub total: u32,
    /// The row this page starts at.
    pub offset: u32,
    /// Rows per page, as observed. Carried rather than hardcoded so the count
    /// line and the paging arithmetic cannot disagree with the wire.
    pub page_rows: u32,
    /// Which row is selected, by **auction id** and not by position: paging
    /// while something is selected must not silently move the selection to
    /// whatever is now in that slot.
    pub selected: Option<u32>,
    /// What the search box holds. Drawn; typed into elsewhere.
    pub search: String,
    /// Whether a request is outstanding, so the window can say so rather than
    /// looking like a list that came back empty.
    pub waiting: bool,
    /// What the auctioneer said its house was. Drawn because it is the only
    /// thing that distinguishes two auctioneers whose windows are otherwise
    /// identical.
    pub house: Option<u32>,
}

impl AuctionView {
    /// The selected row, if it is still on this page.
    ///
    /// `None` after paging away from it, which is correct: the buttons act on
    /// something that is on screen, and a selection that scrolled out of the
    /// match is not.
    pub fn selection(&self) -> Option<&AuctionRow> {
        let id = self.selected?;
        self.rows.iter().find(|row| row.id == id)
    }

    /// One-based page number and page count.
    ///
    /// Derived from [`Self::page_rows`] rather than from `rows.len()`, which
    /// is the *last* page's length on the last page and would report one page
    /// too few every time anybody looked at the end of a match.
    pub fn page(&self) -> (u32, u32) {
        let size = self.page_rows.max(1);
        let pages = if self.total == 0 {
            0
        } else {
            self.total.div_ceil(size)
        };
        (self.offset / size + 1, pages)
    }

    /// Whether there is a page after this one.
    pub fn has_next(&self) -> bool {
        self.tab.pages() && self.offset + (self.rows.len() as u32) < self.total
    }

    /// Whether there is a page before this one.
    pub fn has_previous(&self) -> bool {
        self.tab.pages() && self.offset > 0
    }

    /// The sentence under the header: what is on screen out of what exists.
    ///
    /// **The line this whole frame is about.** Written here rather than inline
    /// in the painter so the test can assert the words rather than a pixel.
    pub fn range_line(&self) -> String {
        if self.waiting {
            return "Asking...".to_string();
        }
        if self.rows.is_empty() {
            // **Past the end is not the same as no match**, and saying
            // "Nothing matched" here would be a lie about the search rather
            // than about the page: narrowing a filter while on page nine
            // leaves the offset where it was and the match far shorter, and
            // the rows are all still there one page back. The naive
            // arithmetic says "page 3 of 1" in this state, which is what
            // this branch exists to stop being drawn.
            if self.offset > 0 && self.total > 0 {
                let (_, pages) = self.page();
                return format!(
                    "Past the end -- {} matched, {} page{}. Go back.",
                    self.total,
                    pages,
                    if pages == 1 { "" } else { "s" }
                );
            }
            return match self.tab {
                AuctionTab::Browse => "Nothing matched.".to_string(),
                AuctionTab::Bids => "You are not bidding on anything.".to_string(),
                AuctionTab::Selling => "You are not selling anything.".to_string(),
            };
        }
        if !self.tab.pages() {
            return format!("{} auctions", self.rows.len());
        }
        let first = self.offset + 1;
        let last = self.offset + self.rows.len() as u32;
        let (page, pages) = self.page();
        if self.total as usize <= self.rows.len() {
            format!("{}-{} of {} -- all of them", first, last, self.total)
        } else {
            format!(
                "{}-{} of {} -- page {} of {}",
                first, last, self.total, page, pages
            )
        }
    }
}

/// A window's worth of plausible rows, for the layout editor.
///
/// **A full [`VISIBLE_ROWS`] of them**, because the editor is where somebody
/// sizes and positions this frame and a short placeholder would size a window
/// for a page that never happens -- the same reason the trainer's placeholder
/// carries its greyed rows. Deliberately a page that is *not* the whole match,
/// and deliberately carrying one of this character's own rows, since those are
/// the two states the window has to get right.
pub fn placeholder() -> AuctionView {
    let row = |id: u32, name: &str, count: u32, bid: u32, next: u32, buyout: u32, band, own| {
        AuctionRow {
            id,
            name: name.into(),
            count,
            icon: None,
            seller: if own { "Testwolf".into() } else { "Watcher".into() },
            bid,
            next_bid: next,
            buyout,
            band,
            own,
        }
    };
    AuctionView {
        tab: AuctionTab::Browse,
        rows: vec![
            row(1041, "Linen Cloth", 20, 0, 500, 5000, TimeBand::VeryLong, false),
            row(1042, "Copper Ore", 20, 733, 770, 0, TimeBand::Long, false),
            row(1043, "Silver Ore", 20, 0, 890, 12000, TimeBand::Medium, false),
            row(1044, "Malachite", 1, 1200, 1260, 9000, TimeBand::Short, false),
            row(1045, "Ice Cold Milk", 5, 0, 300, 1500, TimeBand::VeryLong, true),
            row(1046, "Tin Ore", 20, 0, 610, 4200, TimeBand::Long, false),
            row(1047, "Wool Cloth", 20, 410, 431, 3300, TimeBand::VeryLong, false),
            row(1048, "Silk Cloth", 20, 0, 1500, 0, TimeBand::Medium, false),
            row(1049, "Tigerseye", 1, 0, 220, 1800, TimeBand::Short, false),
            row(1050, "Refreshing Spring Water", 5, 90, 95, 700, TimeBand::Long, false),
            row(1051, "Copper Ore", 20, 0, 640, 5100, TimeBand::VeryLong, false),
            row(1052, "Malachite", 1, 0, 260, 0, TimeBand::Medium, true),
        ],
        total: 1284,
        offset: 48,
        page_rows: VISIBLE_ROWS as u32,
        selected: Some(1042),
        search: "ore".into(),
        waiting: false,
        house: Some(2),
    }
}

/// How many rows this window draws, and therefore **how big a page it asks
/// for**.
///
/// Not fifty, and the difference is worth stating because fifty is the number
/// the server is capped at. `listfrom` is a **row index**, not a page number,
/// so a client chooses its own page size and the server's fifty is a ceiling
/// rather than a step: asking from row 12 returns row 12 onwards. Fifty rows
/// at thirty pixels is 1,500 and does not fit on any screen this client
/// supports, so drawing fifty would mean a window taller than the display or a
/// scrollbar this interface does not have.
///
/// So the window pages by twelve and the server is asked from row `offset`.
/// Everything the range line says stays true, because `total` is the server's
/// number and the page arithmetic is derived from this one.
pub const VISIBLE_ROWS: usize = 12;

/// How much room a window with this many rows wants.
///
/// Clamped to [`VISIBLE_ROWS`]: a fifty-row page must not produce a window
/// fifteen hundred pixels tall.
pub fn size(rows: usize, style: &Style, scale: f32) -> Vec2 {
    let row = style.spellbook_row;
    let drawn = rows.clamp(1, VISIBLE_ROWS) as f32;
    let height = header(style) + drawn * row + footer(style) + style.padding * 2.0;
    Vec2::new(style.loot_width * 2.6, height) * scale
}

/// Unscaled height of the title, the tabs and the range line.
fn header(style: &Style) -> f32 {
    (style.font_size + style.gap) * 3.0
}

/// Unscaled height of the paging row, the buttons and the hint line.
fn footer(style: &Style) -> f32 {
    (style.font_size + style.gap) * 3.0
}

/// Where each tab's label sits.
///
/// Geometry stated once and read by the drawing and the hit test both -- the
/// rule the party invite prompt's two adjacent answers made load-bearing,
/// where a press between the buttons answered nothing.
pub fn tab_rects(rect: Rect, style: &Style, scale: f32) -> impl Iterator<Item = Rect> + '_ {
    let pad = style.padding * scale;
    let top = rect.min.y + pad + (style.font_size + style.gap) * scale;
    let height = (style.font_size + style.gap) * scale;
    let width = (style.loot_width * 0.6) * scale;
    (0..AuctionTab::ALL.len()).map(move |i| {
        Rect::from_min_size(
            Pos2::new(rect.min.x + pad + i as f32 * (width + style.gap * scale), top),
            Vec2::new(width, height),
        )
    })
}

/// Which tab contains a point.
pub fn tab_at(rect: Rect, style: &Style, scale: f32, point: Pos2) -> Option<AuctionTab> {
    tab_rects(rect, style, scale)
        .position(|tab| tab.contains(point))
        .map(|i| AuctionTab::ALL[i])
}

/// Where each row sits.
///
/// The single source of truth for row geometry, used by the drawing and the
/// hit test both.
pub fn row_rects(
    rect: Rect,
    rows: usize,
    style: &Style,
    scale: f32,
) -> impl Iterator<Item = Rect> + '_ {
    let pad = style.padding * scale;
    let row = style.spellbook_row * scale;
    let top = rect.min.y + pad + header(style) * scale;
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    (0..rows).map(move |i| {
        Rect::from_min_size(Pos2::new(left, top + i as f32 * row), Vec2::new(width, row))
    })
}

/// Which row contains a point, **if that row is one a click can act on**.
pub fn row_at(
    rect: Rect,
    rows: &[AuctionRow],
    tab: AuctionTab,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<usize> {
    row_rects(rect, rows.len(), style, scale)
        .position(|row| row.contains(point))
        .filter(|&index| rows[index].selectable(tab))
}

/// What the controls under the list do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuctionClick {
    /// Ask for the page before this one.
    PreviousPage,
    /// Ask for the page after this one.
    NextPage,
    /// Bid the minimum on the selection.
    Bid,
    /// Buy the selection out.
    Buyout,
    /// Cancel the selection -- only on the selling tab.
    Cancel,
}

/// Where each control under the list sits.
///
/// Five slots in a fixed order whether or not each is live, so the buttons do
/// not move under the pointer when a selection changes. A control that shifts
/// position depending on state is one somebody clicks by muscle memory and
/// misses.
pub fn control_rects(rect: Rect, style: &Style, scale: f32) -> [(AuctionClick, Rect); 5] {
    let pad = style.padding * scale;
    let height = (style.font_size + style.gap) * scale;
    let bottom = rect.max.y - pad - height * 2.0;
    let width = (style.loot_width * 0.55) * scale;
    let gap = style.gap * scale;
    let slot = |i: f32| {
        Rect::from_min_size(
            Pos2::new(rect.min.x + pad + i * (width + gap), bottom),
            Vec2::new(width, height),
        )
    };
    [
        (AuctionClick::PreviousPage, slot(0.0)),
        (AuctionClick::NextPage, slot(1.0)),
        (AuctionClick::Bid, slot(2.0)),
        (AuctionClick::Buyout, slot(3.0)),
        (AuctionClick::Cancel, slot(4.0)),
    ]
}

/// Whether a control would do anything, given what is on screen.
///
/// **The one place this is decided**, read by the drawing so a dead control is
/// dimmed and by the hit test so it does not answer. The alternative ships a
/// request the server declines in silence -- the failure this client cannot
/// diagnose, and the reason the trainer window's inert rows do not answer
/// either.
pub fn control_live(view: &AuctionView, click: AuctionClick) -> bool {
    match click {
        AuctionClick::PreviousPage => view.has_previous(),
        AuctionClick::NextPage => view.has_next(),
        AuctionClick::Bid => view
            .selection()
            .is_some_and(|row| !row.own && row.next_bid > 0),
        AuctionClick::Buyout => view
            .selection()
            .is_some_and(|row| !row.own && row.buyout > 0),
        AuctionClick::Cancel => {
            view.tab == AuctionTab::Selling && view.selection().is_some_and(|row| row.own)
        }
    }
}

/// Which control contains a point, if it is one that would act.
pub fn control_at(
    rect: Rect,
    view: &AuctionView,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<AuctionClick> {
    control_rects(rect, style, scale)
        .into_iter()
        .find(|(_, bounds)| bounds.contains(point))
        .map(|(click, _)| click)
        .filter(|&click| control_live(view, click))
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// What a control says, prices included.
///
/// The price is on the button rather than only in the row, so the number a
/// person is agreeing to is under the pointer when they press it.
fn control_label(view: &AuctionView, click: AuctionClick) -> String {
    match click {
        AuctionClick::PreviousPage => "< Previous".to_string(),
        AuctionClick::NextPage => "Next >".to_string(),
        AuctionClick::Bid => match view.selection() {
            Some(row) if !row.own => format!("Bid {}", money(row.next_bid)),
            _ => "Bid".to_string(),
        },
        AuctionClick::Buyout => match view.selection() {
            Some(row) if !row.own && row.buyout > 0 => format!("Buyout {}", money(row.buyout)),
            _ => "Buyout".to_string(),
        },
        AuctionClick::Cancel => "Cancel".to_string(),
    }
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, view: &AuctionView, style: &Style, scale: f32) {
    let corner = corner_radius(style.corner * scale);
    painter.rect_filled(rect, corner, style.spellbook_background);
    if style.border_width > 0.0 {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(style.border_width * scale, style.border),
            StrokeKind::Inside,
        );
    }

    let text: Color32 = style.text.into();
    let pad = style.padding * scale;
    let font = FontId::proportional(style.font_size * scale);
    let small = FontId::proportional(style.font_size * 0.85 * scale);

    let title = match view.house {
        Some(house) => format!("Auction House {house}"),
        None => "Auction House".to_string(),
    };
    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        title,
        font.clone(),
        text,
    );
    // The search text is drawn even when empty, so the window says what it is
    // showing rather than leaving "everything" implied.
    let searching = if view.search.is_empty() {
        "everything".to_string()
    } else {
        format!("\"{}\"", view.search)
    };
    painter.text(
        Pos2::new(rect.max.x - pad, rect.min.y + pad),
        Align2::RIGHT_TOP,
        searching,
        small.clone(),
        dim(text, 0.7),
    );

    for (tab, bounds) in AuctionTab::ALL.into_iter().zip(tab_rects(rect, style, scale)) {
        let on = tab == view.tab;
        if on {
            painter.rect_filled(bounds, corner_radius(2.0 * scale), dim(text, 0.16));
        }
        painter.text(
            bounds.center(),
            Align2::CENTER_CENTER,
            tab.label(),
            small.clone(),
            if on { text } else { dim(text, 0.55) },
        );
    }

    // **The line this frame exists for.** Drawn in every state, including the
    // ones where it is uninteresting, because a line that appears only when
    // there is a surplus is a line nobody has learned to read.
    painter.text(
        Pos2::new(
            rect.min.x + pad,
            rect.min.y + pad + (style.font_size + style.gap) * 2.0 * scale,
        ),
        Align2::LEFT_TOP,
        view.range_line(),
        small.clone(),
        dim(text, 0.8),
    );

    let painter = painter.with_clip_rect(rect);
    for (index, bounds) in row_rects(rect, view.rows.len(), style, scale).enumerate() {
        let Some(row) = view.rows.get(index) else { break };

        let selected = view.selected == Some(row.id);
        if selected {
            painter.rect_filled(bounds, corner_radius(2.0 * scale), dim(text, 0.18));
        }
        let label = if row.selectable(view.tab) {
            text
        } else {
            dim(text, 0.45)
        };

        let side = bounds.height() - style.border_width * 2.0 * scale;
        if let Some(icon) = row.icon {
            painter.image(
                icon,
                Rect::from_min_size(
                    Pos2::new(bounds.min.x, bounds.min.y + style.border_width * scale),
                    Vec2::splat(side),
                ),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                if row.selectable(view.tab) {
                    Color32::WHITE
                } else {
                    Color32::from_gray(110)
                },
            );
        }

        // A stack is bought whole, so the count rides with the name rather
        // than sitting in a column: the price on the right is for all of them.
        let left = bounds.min.x + side + style.gap * scale;
        let name = if row.count > 1 {
            format!("{} x{}", row.name, row.count)
        } else {
            row.name.clone()
        };
        painter.text(
            Pos2::new(left, bounds.min.y + style.gap * scale),
            Align2::LEFT_TOP,
            name,
            font.clone(),
            label,
        );
        painter.text(
            Pos2::new(left, bounds.max.y - style.gap * scale),
            Align2::LEFT_BOTTOM,
            format!("{} -- {}", row.seller, row.band.label()),
            small.clone(),
            dim(label, 0.7),
        );

        let right = bounds.max.x;
        // The current bid, or the word for having none. "0" would read as a
        // price somebody could pay.
        painter.text(
            Pos2::new(right, bounds.min.y + style.gap * scale),
            Align2::RIGHT_TOP,
            if row.bid > 0 {
                money(row.bid)
            } else {
                "no bid".to_string()
            },
            small.clone(),
            label,
        );
        // Blank rather than zero when the seller offered no buyout.
        if row.buyout > 0 {
            painter.text(
                Pos2::new(right, bounds.max.y - style.gap * scale),
                Align2::RIGHT_BOTTOM,
                money(row.buyout),
                small.clone(),
                dim(label, 0.7),
            );
        }
    }

    for (click, bounds) in control_rects(rect, style, scale) {
        let live = control_live(view, click);
        painter.rect_filled(
            bounds,
            corner_radius(2.0 * scale),
            dim(text, if live { 0.18 } else { 0.07 }),
        );
        painter.text(
            bounds.center(),
            Align2::CENTER_CENTER,
            control_label(view, click),
            small.clone(),
            if live { text } else { dim(text, 0.35) },
        );
    }

    // The gesture, named on screen before anybody has had to guess at it.
    let hint = if view.rows.is_empty() {
        "Nothing to act on."
    } else if view.selection().is_some() {
        "Bid and Buyout act on the selected row"
    } else {
        "Click a row to select it"
    };
    painter.text(
        Pos2::new(rect.min.x + pad, rect.max.y - pad),
        Align2::LEFT_BOTTOM,
        hint,
        small,
        dim(text, 0.6),
    );
}

fn dim(colour: Color32, by: f32) -> Color32 {
    crate::frames::trainer::dim(colour, by)
}

/// Copper into the three coin units.
pub fn money(copper: u32) -> String {
    crate::frames::trainer::money(copper)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The line the whole frame is about.** A page that is not the match has
    /// to say so, in words, on screen.
    #[test]
    fn the_range_line_says_the_page_is_not_the_list() {
        let view = placeholder();
        let line = view.range_line();
        assert!(line.contains("49-60"), "{line}");
        assert!(line.contains("1284"), "{line}");
        assert!(line.contains("page 5 of 107"), "{line}");
    }

    /// ...and when the match does fit, it says that instead, rather than
    /// implying a surplus that is not there.
    #[test]
    fn a_whole_match_says_so() {
        let mut view = placeholder();
        view.offset = 0;
        view.total = view.rows.len() as u32;
        let line = view.range_line();
        assert!(line.contains("all of them"), "{line}");
        assert!(!view.has_next());
        assert!(!view.has_previous());
    }

    /// The last page is short, and the page count must not come from it.
    #[test]
    fn the_last_page_does_not_shorten_the_page_count() {
        let mut view = placeholder();
        // Four rows on a page of fifty: the whole point is that the page
        // count comes from the total and the page size, never from how many
        // rows happened to arrive.
        view.offset = 1248;
        view.total = 1252;
        view.rows.truncate(4);
        assert_eq!(view.page(), (105, 105));
        assert!(!view.has_next());
        assert!(view.has_previous());
    }

    /// The two lists that cannot page must not draw paging.
    #[test]
    fn the_owner_and_bidder_tabs_do_not_page() {
        for tab in [AuctionTab::Bids, AuctionTab::Selling] {
            let mut view = placeholder();
            view.tab = tab;
            view.total = 1284;
            assert!(!view.has_next(), "{tab:?}");
            assert!(!view.has_previous(), "{tab:?}");
            assert!(!control_live(&view, AuctionClick::NextPage), "{tab:?}");
            assert!(view.range_line().contains("auctions"), "{tab:?}");
        }
    }

    /// Both halves of the hit test's contract, because asserting only the
    /// first would pass just as well if `row_at` reported every row.
    #[test]
    fn only_a_row_this_character_can_act_on_answers_a_click() {
        let style = Style::default();
        let view = placeholder();
        let rect = Rect::from_min_size(Pos2::ZERO, size(view.rows.len(), &style, 1.0));
        let rects: Vec<Rect> = row_rects(rect, view.rows.len(), &style, 1.0).collect();

        assert_eq!(
            row_at(rect, &view.rows, view.tab, &style, 1.0, rects[0].center()),
            Some(0)
        );
        assert_eq!(
            row_at(rect, &view.rows, view.tab, &style, 1.0, rects[4].center()),
            None,
            "this character's own auction answered a click on the browse tab, and the \
             bid it would send is one the server refuses"
        );
    }

    /// ...and the same row on the selling tab is the only one that *does*
    /// answer. The rule is about the pair, so the test is too.
    #[test]
    fn the_selling_tab_inverts_which_rows_are_live() {
        let style = Style::default();
        let mut view = placeholder();
        view.tab = AuctionTab::Selling;
        let rect = Rect::from_min_size(Pos2::ZERO, size(view.rows.len(), &style, 1.0));
        let rects: Vec<Rect> = row_rects(rect, view.rows.len(), &style, 1.0).collect();
        assert_eq!(
            row_at(rect, &view.rows, view.tab, &style, 1.0, rects[4].center()),
            Some(4)
        );
        assert_eq!(
            row_at(rect, &view.rows, view.tab, &style, 1.0, rects[0].center()),
            None
        );
    }

    /// A selection is an id, so paging cannot silently move it to whatever is
    /// now in that slot.
    #[test]
    fn a_selection_that_paged_away_is_no_selection() {
        let mut view = placeholder();
        assert_eq!(view.selection().map(|row| row.id), Some(1042));
        view.rows.retain(|row| row.id != 1042);
        assert!(view.selection().is_none());
        assert!(!control_live(&view, AuctionClick::Bid));
        assert!(!control_live(&view, AuctionClick::Buyout));
    }

    /// A row with no buyout must not offer one.
    #[test]
    fn an_auction_with_no_buyout_has_no_buyout_button() {
        let mut view = placeholder();
        view.selected = Some(1042); // buyout 0 in the placeholder
        assert!(control_live(&view, AuctionClick::Bid));
        assert!(!control_live(&view, AuctionClick::Buyout));
    }

    /// A dead control must not answer a click, or the window ships a request
    /// the server declines in silence.
    #[test]
    fn a_dead_control_does_not_answer() {
        let style = Style::default();
        let mut view = placeholder();
        view.offset = 0; // no previous page
        let rect = Rect::from_min_size(Pos2::ZERO, size(view.rows.len(), &style, 1.0));
        let controls = control_rects(rect, &style, 1.0);
        let previous = controls[0].1;
        assert_eq!(control_at(rect, &view, &style, 1.0, previous.center()), None);
        let next = controls[1].1;
        assert_eq!(
            control_at(rect, &view, &style, 1.0, next.center()),
            Some(AuctionClick::NextPage)
        );
    }

    /// The price a person agrees to is on the button they press.
    #[test]
    fn the_buttons_name_their_prices() {
        let mut view = placeholder();
        view.selected = Some(1044);
        assert!(control_label(&view, AuctionClick::Bid).contains("Bid "));
        assert!(control_label(&view, AuctionClick::Buyout).contains("Buyout "));
    }

    /// Rows are laid out by one function and nothing else.
    #[test]
    fn rows_do_not_overlap_or_leave_the_window() {
        let style = Style::default();
        let view = placeholder();
        let rect = Rect::from_min_size(Pos2::new(40.0, 30.0), size(view.rows.len(), &style, 1.0));
        let rects: Vec<Rect> = row_rects(rect, view.rows.len(), &style, 1.0).collect();
        for pair in rects.windows(2) {
            assert!(pair[0].max.y <= pair[1].min.y + 0.01);
        }
        for row in &rects {
            assert!(rect.contains_rect(*row), "{row:?} is outside {rect:?}");
        }
    }

    /// The controls stay clear of the rows at every list length, the same
    /// assertion the mailbox's hint band needed after it grew towards the
    /// click targets.
    #[test]
    fn the_controls_never_cover_a_row() {
        let style = Style::default();
        for count in 0..12 {
            let rect = Rect::from_min_size(Pos2::ZERO, size(count, &style, 1.0));
            let band = control_rects(rect, &style, 1.0)[0].1.min.y;
            for row in row_rects(rect, count, &style, 1.0) {
                assert!(
                    row.max.y <= band + 0.01,
                    "row {row:?} reaches into the control band at {band} with {count} rows"
                );
            }
        }
    }

    /// The tabs do not overlap each other or the rows.
    #[test]
    fn the_tabs_stay_above_the_rows() {
        let style = Style::default();
        let view = placeholder();
        let rect = Rect::from_min_size(Pos2::ZERO, size(view.rows.len(), &style, 1.0));
        let tabs: Vec<Rect> = tab_rects(rect, &style, 1.0).collect();
        for pair in tabs.windows(2) {
            assert!(pair[0].max.x <= pair[1].min.x + 0.01);
        }
        let first_row = row_rects(rect, view.rows.len(), &style, 1.0).next().unwrap();
        for tab in &tabs {
            assert!(tab.max.y <= first_row.min.y + 0.01);
            assert_eq!(tab_at(rect, &style, 1.0, tab.center()).is_some(), true);
        }
    }

    /// An offset past the end of the match must not print "page 3 of 1".
    ///
    /// The state is reachable without carelessness -- narrow the search while
    /// on a later page -- and the probe printed exactly that nonsense before
    /// this existed.
    #[test]
    fn a_page_past_the_end_says_so_rather_than_counting_past_the_last_one() {
        let mut view = placeholder();
        view.offset = 100;
        view.total = 39;
        view.rows.clear();
        let line = view.range_line();
        assert!(line.contains("Past the end"), "{line}");
        assert!(line.contains("39 matched"), "{line}");
        assert!(!line.contains("page 9"), "{line}");
        // ...and the way back is still offered, which is the whole point of
        // saying so rather than of hiding it.
        assert!(view.has_previous());
        assert!(!view.has_next());
    }

    /// An outstanding request says so, rather than looking like an empty
    /// result -- the two are the same picture and different facts.
    #[test]
    fn waiting_is_not_the_same_as_nothing() {
        let mut view = placeholder();
        view.waiting = true;
        assert_eq!(view.range_line(), "Asking...");
        view.waiting = false;
        view.rows.clear();
        // From the start of the match, so this is a search that matched
        // nothing rather than a page past the end -- the two are different
        // sentences and the next test asserts the other one.
        view.offset = 0;
        assert_eq!(view.range_line(), "Nothing matched.");
    }
}
