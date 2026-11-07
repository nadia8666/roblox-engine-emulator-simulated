//! The terminal: raw mode on the alternate screen, keys in, the screen drawn after each, and the terminal put back on
//! the way out (a panic included). `y` copies through the terminal's clipboard escape (OSC 52).

use std::io::{Write as _, stdout};

use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
};

use crate::{app::App, keys::Key, theme, ui};

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen, cursor::Show);
}

/// A key the views take, None for the rest
fn key_of(event: event::KeyEvent) -> Option<Key> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    Some(match event.code {
        KeyCode::Char('c') if ctrl => Key::Interrupt,
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        _ => return None,
    })
}

/// Base64, for the clipboard escape
pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Runs the viewer until q (or ctrl-c)
pub fn run(app: &mut App) -> Result<(), String> {
    enable_raw_mode().map_err(|error| format!("the terminal: {error}"))?;
    if let Err(error) = execute!(stdout(), EnterAlternateScreen, cursor::Hide) {
        restore();
        return Err(format!("the terminal: {error}"));
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    let result = (|| -> Result<(), String> {
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout())).map_err(|error| error.to_string())?;
        // (24-bit colour where the terminal shows it, else each colour's nearest of the 256 every terminal has)
        let truecolor = theme::truecolor();
        loop {
            terminal
                .draw(|frame| {
                    ui::draw(frame, app);
                    if !truecolor {
                        for cell in frame.buffer_mut().content.iter_mut() {
                            cell.fg = theme::to_256(cell.fg);
                            cell.bg = theme::to_256(cell.bg);
                        }
                    }
                })
                .map_err(|error| error.to_string())?;
            if let Some(text) = app.clipboard.take() {
                let mut out = stdout();
                let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
                let _ = out.flush();
            }
            if app.quit {
                return Ok(());
            }
            match event::read().map_err(|error| error.to_string())? {
                Event::Key(key) => {
                    if let Some(key) = key_of(key) {
                        app.key(key);
                    }
                }
                Event::Resize(..) => {}
                _ => {}
            }
        }
    })();
    restore();
    let _ = std::panic::take_hook();
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_pads_as_the_standard_does() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64("Grid.luau:32".as_bytes()), "R3JpZC5sdWF1OjMy");
    }
}
