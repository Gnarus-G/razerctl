# razer-poll

Minimal Linux/Rust utility that sets polling rate and DPI without using the
OpenRazer daemon for these mice:

- Razer DeathAdder V2 (`1532:0084`)
- Razer DeathAdder V3 Pro wireless (`1532:00c3`)

## Build on Arch Linux

```bash
sudo pacman -S --needed rust libudev
cargo build --release
```

## First test

Stop OpenRazer so it does not race this utility:

```bash
systemctl --user stop openrazer-daemon.service 2>/dev/null || true
sudo systemctl stop openrazer-daemon.service 2>/dev/null || true
```

List the Razer HID interfaces:

```bash
sudo ./target/release/razer-poll --list
```

Read the current DPI and polling rate from both mice:

```bash
sudo ./target/release/razer-poll --status
```

Set every connected supported mouse to 1000 Hz:

```bash
sudo ./target/release/razer-poll 1000
```

Set every connected supported mouse's DPI:

```bash
sudo ./target/release/razer-poll --dpi 1600
```

Set both in one invocation:

```bash
sudo ./target/release/razer-poll 1000 --dpi 1600
```

Restrict any operation to one model by adding its product ID:

```bash
sudo ./target/release/razer-poll --pid 0084 1000 --dpi 1600
```

Accepted polling rates for both models are `125`, `500`, and `1000` Hz.
Accepted DPI ranges are `100`-`20000` for the DeathAdder V2 and `100`-`35000`
for the DeathAdder V3 Pro; when both are connected, the shared maximum is
`20000`.

## Run without sudo

```bash
sudo install -Dm644 99-razer-poll.rules /etc/udev/rules.d/99-razer-poll.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Unplug and reconnect the dongle after installing the rule.

## Notes

- Operations target every connected supported mouse unless `--pid` is supplied.
- Use `--pid 0084` for only the DeathAdder V2 or `--pid 00c3` for only the
  DeathAdder V3 Pro wireless.
- The tool targets HID interface 0 by default.
- Use `--interface` only after checking `--list`.
- Firmware may reset settings after reboot or reconnection; run the utility from
  a user systemd service if necessary.
- The utility follows the current OpenRazer command sequences for both models.
