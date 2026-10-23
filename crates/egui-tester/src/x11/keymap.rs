use x11rb::{connection::Connection as _, protocol::xproto::ConnectionExt as _};
use xkeysym::Keysym;

use crate::{Error, Key, Result};

use super::{X11Controller, x11};

/// `NoSymbol`: an empty keymap level.
const NO_SYMBOL: u32 = 0;

/// Keysyms of vendor keys (`XF86…`), outside the Latin-1 and named keys a
/// story can strike.
const VENDOR: std::ops::RangeInclusive<u32> = 0x1008_FF00..=0x1008_FFFF;

/// The server's core keymap: one row of levels per keycode from `first`.
struct Keymap {
    first: u8,
    stride: usize,
    keysyms: Vec<u32>,
}

impl Keymap {
    /// Each keycode with its group-one levels, unshifted then shifted.
    fn rows(&self) -> impl Iterator<Item = (u8, &[u32])> {
        (self.first..=u8::MAX)
            .zip(self.keysyms.chunks(self.stride))
            .map(|(keycode, row)| (keycode, &row[..row.len().min(2)]))
    }

    /// Keycodes no story can strike: those binding nothing, then those
    /// binding only vendor symbols, which [`Key`] cannot name.
    fn spares(&self) -> impl Iterator<Item = u8> {
        let rows = || (self.first..=u8::MAX).zip(self.keysyms.chunks(self.stride));
        let empty = rows()
            .filter(|(_, row)| row.iter().all(|&symbol| symbol == NO_SYMBOL))
            .map(|(keycode, _)| keycode);
        let vendor = rows()
            .filter(|(_, row)| {
                row.iter().any(|symbol| VENDOR.contains(symbol))
                    && row
                        .iter()
                        .all(|&symbol| symbol == NO_SYMBOL || VENDOR.contains(&symbol))
            })
            .map(|(keycode, _)| keycode);
        empty.chain(vendor)
    }
}

impl X11Controller {
    /// The keycode typing `symbol` in group one, and whether Shift selects it. Other
    /// groups are unreachable: injected strokes never switch group.
    pub(super) fn keycode(&self, symbol: Keysym) -> Result<(u8, bool)> {
        let keymap = self.keymap()?;
        for (keycode, levels) in keymap.rows() {
            if let Some(level) = levels
                .iter()
                .position(|&candidate| candidate == symbol.raw())
            {
                return Ok((keycode, level == 1));
            }
        }
        Err(Error::Unsupported {
            capability: "keyboard symbol",
            detail: format!("the X11 keymap has no binding for {symbol:?}"),
        })
    }

    /// Bind every symbol group one cannot type to a free keycode, two Shift
    /// levels per keycode. Lent bindings stay for the private server's life:
    /// a client rereads the whole keymap when told it changed, so restoring a
    /// binding after its stroke could reach the client first and the stroke
    /// would type nothing. The server dies with its testbed.
    pub(super) fn lend(&self, symbols: impl IntoIterator<Item = Keysym>) -> Result<()> {
        let keymap = self.keymap()?;
        let mut missing = Vec::new();
        for symbol in symbols {
            let raw = symbol.raw();
            if !missing.contains(&raw) && !keymap.rows().any(|(_, levels)| levels.contains(&raw)) {
                missing.push(raw);
            }
        }
        let mut spares = keymap.spares();
        for pair in missing.chunks(2) {
            let keycode = spares.next().ok_or_else(|| Error::Unsupported {
                capability: "keyboard symbol",
                detail: format!(
                    "the private keymap has no free keycode for {:?}",
                    Keysym::new(pair[0])
                ),
            })?;
            let levels = [pair[0], pair.get(1).copied().unwrap_or(NO_SYMBOL)];
            self.connection
                .change_keyboard_mapping(1, keycode, 2, &levels)
                .map_err(|err| x11("lend keycode", err))?
                .check()
                .map_err(|err| x11("lend keycode", err))?;
        }
        Ok(())
    }

    fn keymap(&self) -> Result<Keymap> {
        let setup = self.connection.setup();
        let count = setup.max_keycode - setup.min_keycode + 1;
        let mapping = self
            .connection
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(|err| x11("read keyboard map", err))?
            .reply()
            .map_err(|err| x11("read keyboard map", err))?;
        Ok(Keymap {
            first: setup.min_keycode,
            stride: usize::from(mapping.keysyms_per_keycode).max(1),
            keysyms: mapping.keysyms,
        })
    }
}

impl Key {
    pub(super) fn keysym(self) -> Result<Keysym> {
        Ok(match self {
            Self::Character(character) => {
                if character.is_control() {
                    return Err(Error::Unsupported {
                        capability: "text input",
                        detail: format!("{character:?} is a control character; send its named Key"),
                    });
                }
                if u32::from(character) > 0xff {
                    return Err(Error::Unsupported {
                        capability: "text input",
                        detail: format!(
                            "`{character}` is outside the MVP's Latin-1 X11 key injection"
                        ),
                    });
                }
                Keysym::new(u32::from(character))
            }
            Self::Space => Keysym::new(u32::from(' ')),
            Self::Return => Keysym::Return,
            Self::Escape => Keysym::Escape,
            Self::Tab => Keysym::Tab,
            Self::Backspace => Keysym::BackSpace,
            Self::Delete => Keysym::Delete,
            Self::Home => Keysym::Home,
            Self::End => Keysym::End,
            Self::Left => Keysym::Left,
            Self::Right => Keysym::Right,
            Self::Up => Keysym::Up,
            Self::Down => Keysym::Down,
            Self::Function(number) => match number {
                1 => Keysym::F1,
                2 => Keysym::F2,
                3 => Keysym::F3,
                4 => Keysym::F4,
                5 => Keysym::F5,
                6 => Keysym::F6,
                7 => Keysym::F7,
                8 => Keysym::F8,
                9 => Keysym::F9,
                10 => Keysym::F10,
                11 => Keysym::F11,
                12 => Keysym::F12,
                _ => {
                    return Err(Error::Unsupported {
                        capability: "function key",
                        detail: format!("F{number} is outside the portable F1–F12 set"),
                    });
                }
            },
            Self::Shift => Keysym::Shift_L,
            Self::Control => Keysym::Control_L,
            Self::Alt => Keysym::Alt_L,
            Self::Super => Keysym::Super_L,
        })
    }
}
