use libghostty_vt::render::{Color, Frame, RenderState, RenderedCell, Terminal};

fn cell_at(frame: &Frame<'_>, x: u16, y: u16) -> RenderedCell {
    frame
        .row(y)
        .unwrap_or_else(|| panic!("missing row {y}"))
        .cell(x)
        .unwrap_or_else(|| panic!("missing cell {x},{y}"))
        .clone()
}

#[test]
fn cell_readback_reports_colors_flags_and_wide_cells() {
    let mut terminal = Terminal::new(10, 3).expect("terminal creation");
    let mut state = RenderState::new().expect("render state creation");

    terminal.write(b"\x1b[31mRED\x1b[0m");
    state.update(&mut terminal);
    {
        let frame = state.frame();
        assert_eq!(frame.size(), (3, 10));
        assert_eq!(frame.rows().count(), 3);
        assert!(frame.total_rows() >= 3);
        let blank = cell_at(&frame, 9, 2);
        assert_eq!(blank.fg, Color::Default);
        assert_eq!(blank.bg, Color::Default);
        for (x, expected) in [(0u16, 'R'), (1, 'E'), (2, 'D')] {
            let cell = cell_at(&frame, x, 0);
            assert_eq!(cell.graphemes, expected.to_string());
            assert_eq!(cell.fg, Color::Indexed(1), "cell {x} foreground");
        }
    }

    terminal.write(b"\x1b[38;2;10;20;30mX");
    state.update(&mut terminal);
    {
        let frame = state.frame();
        let cell = cell_at(&frame, 3, 0);
        assert_eq!(cell.graphemes, "X");
        assert_eq!(cell.fg, Color::Rgb(10, 20, 30));
    }

    terminal.write(b"\x1b[1m\x1b[7mB");
    state.update(&mut terminal);
    {
        let frame = state.frame();
        let cell = cell_at(&frame, 4, 0);
        assert_eq!(cell.graphemes, "B");
        assert!(cell.bold, "bold flag");
        assert!(cell.inverse, "inverse flag");
    }

    terminal.write("漢".as_bytes());
    state.update(&mut terminal);
    {
        let frame = state.frame();
        let head = cell_at(&frame, 5, 0);
        assert_eq!(head.graphemes, "漢");
        assert!(head.wide, "wide head cell");
        assert!(!head.wide_continuation, "head is not a continuation");

        let tail = cell_at(&frame, 6, 0);
        assert!(tail.wide_continuation, "wide continuation cell");
        assert!(!tail.wide, "tail is not the wide head");
    }

    terminal.write(b"\x1b[2;3H");
    state.update(&mut terminal);
    {
        let frame = state.frame();
        let cursor = frame.cursor();
        assert_eq!((cursor.x, cursor.y), (2, 1));
    }
}
