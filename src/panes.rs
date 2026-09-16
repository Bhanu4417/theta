//! Pane grid: rows of cells with adjustable weights.
//!
//! Layouts are tmux-like: adjacent panes share their border line, so the grid
//! produces single separators between panes. Auto-tiling picks a column count
//! from the terminal aspect ratio (cells ~2x taller than wide).

use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub weight: f32,
    pub session: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub weight: f32,
    pub cells: Vec<Cell>,
}

/// Tiling scheme the workspace uses (Ctrl+T to switch).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scheme {
    /// Adaptive grid (columns from terminal aspect).
    #[default]
    Auto,
    /// Every pane stacked vertically, full width.
    Rows,
    /// All panes side by side.
    Columns,
}

impl Scheme {
    pub fn label(&self) -> &'static str {
        match self {
            Scheme::Auto => "Auto tile",
            Scheme::Rows => "Rows (stacked)",
            Scheme::Columns => "Columns (side by side)",
        }
    }

    pub fn all() -> [Scheme; 3] {
        [Scheme::Auto, Scheme::Rows, Scheme::Columns]
    }

    pub fn from_str(s: &str) -> Option<Scheme> {
        match s {
            "auto" => Some(Scheme::Auto),
            "rows" => Some(Scheme::Rows),
            "columns" => Some(Scheme::Columns),
            _ => None,
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            Scheme::Auto => "auto",
            Scheme::Rows => "rows",
            Scheme::Columns => "columns",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PaneGrid {
    pub rows: Vec<Row>,
    pub scheme: Scheme,
}

pub const MIN_PANE_W: u16 = 24;
pub const MIN_PANE_H: u16 = 8;

impl PaneGrid {
    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|r| r.cells.is_empty())
    }

    /// Test-only helper: total panes across all rows.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.rows.iter().map(|r| r.cells.len()).sum()
    }

    /// Test-only helper: is this session currently in the grid?
    #[cfg(test)]
    pub fn contains(&self, session: u32) -> bool {
        self.rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.session == session))
    }

    /// Build an auto-tiled grid from a session order.
    pub fn auto(order: &[u32], area_w: u16, area_h: u16) -> Self {
        let mut grid = PaneGrid::default();
        if order.is_empty() {
            return grid;
        }
        let n = order.len();
        let cols = auto_cols(n, area_w, area_h);
        let mut idx = 0;
        while idx < n {
            let take = (cols).min(n - idx);
            let row = Row {
                weight: 1.0,
                cells: order[idx..idx + take]
                    .iter()
                    .map(|s| Cell {
                        weight: 1.0,
                        session: *s,
                    })
                    .collect(),
            };
            grid.rows.push(row);
            idx += take;
        }
        grid
    }

    /// Reset weights to the auto-tile shape, preserving session order.
    pub fn retile(&mut self, area_w: u16, area_h: u16) {
        let order = self.order();
        let scheme = self.scheme;
        *self = Self::build(&order, scheme, area_w, area_h);
    }

    /// Apply a new tiling scheme, preserving session order.
    pub fn apply_scheme(&mut self, scheme: Scheme, area_w: u16, area_h: u16) {
        self.scheme = scheme;
        self.retile(area_w, area_h);
    }

    /// Build a grid in the given scheme from a session order.
    pub fn build(order: &[u32], scheme: Scheme, area_w: u16, area_h: u16) -> Self {
        match scheme {
            Scheme::Auto => Self::auto(order, area_w, area_h),
            Scheme::Rows => PaneGrid {
                scheme,
                rows: order
                    .iter()
                    .map(|sid| Row {
                        weight: 1.0,
                        cells: vec![Cell {
                            weight: 1.0,
                            session: *sid,
                        }],
                    })
                    .collect(),
            },
            Scheme::Columns => PaneGrid {
                scheme,
                rows: vec![Row {
                    weight: 1.0,
                    cells: order
                        .iter()
                        .map(|sid| Cell {
                            weight: 1.0,
                            session: *sid,
                        })
                        .collect(),
                }],
            },
        }
    }

    /// Session ids in z-order (row by row).
    pub fn order(&self) -> Vec<u32> {
        self.rows
            .iter()
            .flat_map(|r| r.cells.iter().map(|c| c.session))
            .collect()
    }

    pub fn normalize(&mut self) {
        self.rows.retain(|r| !r.cells.is_empty());
        for r in &mut self.rows {
            let sum: f32 = r.cells.iter().map(|c| c.weight).sum();
            if sum > 0.0 {
                for c in &mut r.cells {
                    c.weight /= sum;
                }
            }
        }
        let sum: f32 = self.rows.iter().map(|r| r.weight).sum();
        if sum > 0.0 {
            for r in &mut self.rows {
                r.weight /= sum;
            }
        }
    }

    pub fn insert_session(&mut self, session: u32, area_w: u16, area_h: u16) {
        if self.is_empty() {
            *self = Self::build(&[session], self.scheme, area_w, area_h);
            return;
        }
        let mut order = self.order();
        order.push(session);
        *self = Self::build(&order, self.scheme, area_w, area_h);
    }

    pub fn remove_session(&mut self, session: u32, area_w: u16, area_h: u16) {
        let mut order = self.order();
        order.retain(|s| *s != session);
        *self = Self::build(&order, self.scheme, area_w, area_h);
    }

    /// Rects for every session; adjacent panes share separator lines.
    /// Returns None when the area cannot fit all panes at minimum size.
    pub fn rects(&self, area: Rect) -> Option<Vec<(u32, Rect)>> {
        if self.rows.is_empty() || area.width == 0 || area.height == 0 {
            return Some(Vec::new());
        }
        let n_rows = self.rows.len();
        let row_sum: f32 = self.rows.iter().map(|r| r.weight).sum();
        if row_sum <= 0.0 {
            return None;
        }
        // Total border allowance: rows share horizontal borders.
        let avail_h = area.height.saturating_sub((n_rows - 1) as u16) as f32;
        let avail_w = area.width as f32;

        let mut out = Vec::new();
        let mut y = area.y;
        let mut used_h = 0u16;
        for (ri, row) in self.rows.iter().enumerate() {
            let is_last = ri == n_rows - 1;
            let mut h = if is_last {
                area.bottom().saturating_sub(y)
            } else {
                ((row.weight / row_sum) * avail_h).round() as u16
            };
            if h == 0 {
                h = 1;
            }
            let cells = &row.cells;
            let cell_sum: f32 = cells.iter().map(|c| c.weight).sum();
            if cell_sum <= 0.0 || cells.is_empty() {
                y += h + 1;
                used_h += h + 1;
                continue;
            }
            let n_cells = cells.len();
            let cell_avail_w = avail_w - (n_cells - 1) as f32;
            let mut x = area.x;
            for (ci, cell) in cells.iter().enumerate() {
                let is_last_cell = ci == n_cells - 1;
                let w = if is_last_cell {
                    area.right().saturating_sub(x)
                } else {
                    ((cell.weight / cell_sum) * cell_avail_w).round().max(1.0) as u16
                };
                out.push((
                    cell.session,
                    Rect {
                        x,
                        y,
                        width: w.min(area.right().saturating_sub(x)),
                        height: h.min(area.bottom().saturating_sub(y)),
                    },
                ));
                x += w + 1; // +1: shared vertical separator
            }
            y += h + 1; // +1: shared horizontal separator
            used_h += h + 1;
        }
        let _ = used_h;

        // Minimum size check.
        for (_, r) in &out {
            if r.width < MIN_PANE_W || r.height < MIN_PANE_H {
                return None;
            }
        }
        Some(out)
    }

    fn cell_pos(&self, session: u32) -> Option<(usize, usize)> {
        for (ri, r) in self.rows.iter().enumerate() {
            for (ci, c) in r.cells.iter().enumerate() {
                if c.session == session {
                    return Some((ri, ci));
                }
            }
        }
        None
    }

    /// Cyclic neighbour used only for keyboard focus. Horizontal moves follow
    /// row-major session order; vertical moves follow column-major order. In
    /// a regular grid this preserves the adjacent move, while repeated arrows
    /// eventually reach every pane.
    pub fn focus_step(&self, session: u32, dir: Dir) -> Option<u32> {
        match dir {
            Dir::Left | Dir::Right => {
                let flat = self.order();
                if flat.len() <= 1 {
                    return None;
                }
                let pos = flat.iter().position(|s| *s == session)?;
                Some(if dir == Dir::Right {
                    flat[(pos + 1) % flat.len()]
                } else {
                    flat[(pos + flat.len() - 1) % flat.len()]
                })
            }
            Dir::Up | Dir::Down => {
                let max_cols = self
                    .rows
                    .iter()
                    .map(|row| row.cells.len())
                    .max()
                    .unwrap_or(0);
                let mut columns: Vec<Vec<u32>> = vec![Vec::new(); max_cols];
                for row in &self.rows {
                    for (col, cell) in row.cells.iter().enumerate() {
                        columns[col].push(cell.session);
                    }
                }
                let col_index = columns
                    .iter()
                    .position(|column| column.contains(&session))?;
                let column = &columns[col_index];
                let pos = column.iter().position(|s| *s == session)?;
                Some(if dir == Dir::Down {
                    if pos + 1 < column.len() {
                        column[pos + 1]
                    } else {
                        columns[(col_index + 1) % columns.len()][0]
                    }
                } else if pos > 0 {
                    column[pos - 1]
                } else {
                    let prev = &columns[(col_index + columns.len() - 1) % columns.len()];
                    prev[prev.len() - 1]
                })
            }
            Dir::Next | Dir::Prev => None,
        }
    }

    /// Neighbouring session in a direction (grid adjacency for
    /// Left/Right/Up/Down, z-order cycling for Next/Prev).
    pub fn neighbor(&self, session: u32, dir: Dir, rects: &[(u32, Rect)]) -> Option<u32> {
        let _ = rects;
        let (row, col) = self.cell_pos(session)?;
        match dir {
            Dir::Left => {
                if col == 0 {
                    None
                } else {
                    Some(self.rows[row].cells[col - 1].session)
                }
            }
            Dir::Right => {
                if col + 1 >= self.rows[row].cells.len() {
                    None
                } else {
                    Some(self.rows[row].cells[col + 1].session)
                }
            }
            Dir::Up => {
                if row == 0 {
                    None
                } else {
                    let cells = &self.rows[row - 1].cells;
                    Some(cells[col.min(cells.len() - 1)].session)
                }
            }
            Dir::Down => {
                if row + 1 >= self.rows.len() {
                    None
                } else {
                    let cells = &self.rows[row + 1].cells;
                    Some(cells[col.min(cells.len() - 1)].session)
                }
            }
            Dir::Next | Dir::Prev => {
                let flat: Vec<u32> = self.order();
                let pos = flat.iter().position(|s| *s == session)?;
                if dir == Dir::Next {
                    Some(flat[(pos + 1) % flat.len()])
                } else {
                    Some(flat[(pos + flat.len() - 1) % flat.len()])
                }
            }
        }
    }

    /// Adjust weights: grow the focused pane/row toward `dir`
    /// (the neighbour in that direction loses space).
    pub fn resize(&mut self, session: u32, dir: Dir, delta: f32) -> bool {
        let Some((row, col)) = self.cell_pos(session) else {
            return false;
        };
        let clampw = |w: f32| w.clamp(0.08, 0.92);
        match dir {
            Dir::Left | Dir::Right => {
                let neigh = match dir {
                    Dir::Left => {
                        if col == 0 {
                            return false;
                        }
                        col - 1
                    }
                    Dir::Right => {
                        if col + 1 >= self.rows[row].cells.len() {
                            return false;
                        }
                        col + 1
                    }
                    _ => unreachable!(),
                };
                self.rows[row].cells[col].weight = clampw(self.rows[row].cells[col].weight + delta);
                self.rows[row].cells[neigh].weight = clampw(self.rows[row].cells[neigh].weight - delta);
                self.normalize();
                true
            }
            Dir::Up | Dir::Down => {
                let neigh = match dir {
                    Dir::Up => {
                        if row == 0 {
                            return false;
                        }
                        row - 1
                    }
                    Dir::Down => {
                        if row + 1 >= self.rows.len() {
                            return false;
                        }
                        row + 1
                    }
                    _ => unreachable!(),
                };
                self.rows[row].weight = clampw(self.rows[row].weight + delta);
                self.rows[neigh].weight = clampw(self.rows[neigh].weight - delta);
                self.normalize();
                true
            }
            _ => false,
        }
    }

    /// Swap the focused pane with its neighbour in a direction.
    pub fn swap(&mut self, session: u32, dir: Dir, rects: &[(u32, Rect)]) -> bool {
        let Some(other) = self.neighbor(session, dir, rects) else {
            return false;
        };
        if dir == Dir::Next || dir == Dir::Prev {
            return false;
        }
        let Some((orow, ocol)) = self.cell_pos(other) else {
            return false;
        };
        let Some((row, col)) = self.cell_pos(session) else {
            return false;
        };
        self.rows[row].cells[col].session = other;
        self.rows[orow].cells[ocol].session = session;
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
    Next,
    Prev,
}

/// Column count from pane count and terminal aspect (cells ≈ 1:2).
/// Floor keeps the spec layouts: 4 → 2x2, 5 → 3+2, 6 → 3x2.
pub fn auto_cols(n: usize, w: u16, h: u16) -> usize {
    if n <= 1 {
        return 1;
    }
    let ratio = if h == 0 {
        2.0
    } else {
        (w as f32 / 2.0) / h as f32
    };
    let ideal = (n as f32 * ratio.max(0.5)).sqrt() as usize;
    ideal.clamp(1, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_grid_keeps_order_and_covers_every_session() {
        let grid = PaneGrid::auto(&[1, 2, 3, 4], 160, 40);
        assert_eq!(grid.order(), vec![1, 2, 3, 4]);
        let rects = grid.rects(Rect::new(0, 0, 160, 40)).expect("grid fits");
        assert_eq!(rects.len(), 4);
    }

    #[test]
    fn rows_scheme_is_one_pane_per_row() {
        let grid = PaneGrid::build(&[7, 8, 9], Scheme::Rows, 160, 40);
        assert_eq!(grid.rows.len(), 3);
        assert_eq!(grid.len(), 3);
    }

    #[test]
    fn insert_and_remove_session() {
        let mut grid = PaneGrid::auto(&[1], 100, 30);
        grid.insert_session(2, 100, 30);
        assert!(grid.contains(1) && grid.contains(2));
        grid.remove_session(1, 100, 30);
        assert!(!grid.contains(1));
        assert_eq!(grid.order(), vec![2]);
    }

    fn two_by_two_grid() -> PaneGrid {
        PaneGrid {
            rows: vec![
                Row {
                    weight: 1.0,
                    cells: vec![
                        Cell { weight: 1.0, session: 1 },
                        Cell { weight: 1.0, session: 2 },
                    ],
                },
                Row {
                    weight: 1.0,
                    cells: vec![
                        Cell { weight: 1.0, session: 3 },
                        Cell { weight: 1.0, session: 4 },
                    ],
                },
            ],
            scheme: Scheme::Auto,
        }
    }

    #[test]
    fn focus_step_cycles_horizontally_through_every_pane() {
        let grid = two_by_two_grid();
        assert_eq!(grid.focus_step(1, Dir::Right), Some(2));
        assert_eq!(grid.focus_step(2, Dir::Right), Some(3));
        assert_eq!(grid.focus_step(4, Dir::Right), Some(1));
        assert_eq!(grid.focus_step(1, Dir::Left), Some(4));
    }

    #[test]
    fn focus_step_cycles_vertically_through_every_pane() {
        let grid = two_by_two_grid();
        assert_eq!(grid.focus_step(1, Dir::Down), Some(3));
        assert_eq!(grid.focus_step(3, Dir::Down), Some(2));
        assert_eq!(grid.focus_step(2, Dir::Down), Some(4));
        assert_eq!(grid.focus_step(4, Dir::Down), Some(1));
        assert_eq!(grid.focus_step(1, Dir::Up), Some(4));
        assert_eq!(grid.focus_step(4, Dir::Up), Some(2));
    }

    #[test]
    fn focus_step_handles_ragged_rows() {
        let grid = PaneGrid {
            rows: vec![
                Row {
                    weight: 1.0,
                    cells: vec![Cell { weight: 1.0, session: 1 }],
                },
                Row {
                    weight: 1.0,
                    cells: vec![
                        Cell { weight: 1.0, session: 2 },
                        Cell { weight: 1.0, session: 3 },
                    ],
                },
            ],
            scheme: Scheme::Auto,
        };
        assert_eq!(grid.focus_step(1, Dir::Right), Some(2));
        assert_eq!(grid.focus_step(2, Dir::Down), Some(3));
        assert_eq!(grid.focus_step(3, Dir::Down), Some(1));
    }
}
