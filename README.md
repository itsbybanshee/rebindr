<p align="center">
  <img src="assets/icon-256.png" width="96" alt="rebindr icon">
</p>

<h1 align="center">rebindr</h1>

<p align="center"><b>Turn controller buttons into desktop shortcuts.</b></p>

---

rebindr is a tiny Windows app that maps your controller's buttons and combos to keyboard shortcuts, or to opening an app, file or URL. Open it, press **Add**, press a button on your controller and the shortcut you want, and you're done. It then sits quietly in the tray.

It's built in Rust and runs natively: a single ~500 KB `.exe` that uses about 3–4 MB of memory and barely any CPU.

## Why rebindr

- **Record, don't configure.** Press the controller button to record it, then press the keyboard shortcut. Any chord works, even ones Windows normally reserves.
- **Combos and long presses.** Hold View like a modifier, or give one button two jobs: a quick press runs one shortcut, a hold runs another.
- **Share button support.** On the GameSir Nova 2 Lite, Share can be mapped too. rebindr blocks its built-in Game Bar screenshot while a shortcut uses it.
- **Name and share your setups.** Name each shortcut, and **Import / Export** your set as a `.json` file to share with others.
- **Lightweight.** One small app with no services, no installer, no account and no internet access.
- **Works with XInput controllers.** Xbox One and Xbox Series controllers, the GameSir Nova 2 Lite and other XInput pads.

## Install

1. Download `rebindr.exe` from the latest release.
2. Run it. rebindr opens its window and adds an icon to the tray.
3. Optionally, tick **Launch at sign-in**.

Closing the window keeps your shortcuts running. Left-click the tray icon to reopen it, or right-click it to turn shortcuts off or quit.

## Use

- **Add:** give the shortcut a name if you like, then **Record** the controller button(s) or type them (`View + Share`). Choose **Press** or **Long press**, then **Record keys** or type the shortcut (`Ctrl + Shift + E`). You can do these in any order.
- **Remove:** click the red **✕** on a shortcut.
- **Enabled:** turn every shortcut off at once, for example while gaming.

Games still receive the controller buttons as usual.

## Build

Requires [Rust](https://rustup.rs).

```powershell
cargo build --release
```

```powershell
cargo test --release -- --test-threads=1
```

## Notes

- Settings live in `%APPDATA%\rebindr\config.json`.
- The Xbox Series controller's Share button is reserved by Windows for Game Bar, so apps can't see it. The GameSir Nova 2 Lite's Share works because its dongle sends it as a keystroke.
- Windows doesn't let apps simulate **Win + L** or **Ctrl + Alt + Del**. Shortcuts don't reach admin windows unless rebindr also runs as admin.
- Other keyboard tools (for example PowerToys Keyboard Manager) can intercept combos while you record them. If a combo is missing a key, type it into the Keys box instead.
