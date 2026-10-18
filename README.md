# sr5900p

A command-line interface and library for SR5900P tape printer.

This is a fork of [hikalium/sr5900p](https://github.com/hikalium/sr5900p).

![](./assets/test_pattern_18mm.png)
![](./assets/qr_text_18mm.png)

```
# Install
make install

# Preview - preview.png will be generated after each commands:
sr5900p print --dry-run --width 36 --test-pattern
sr5900p print --dry-run --width 12 --test-pattern
sr5900p print --dry-run --width 12 --qr-text 'Hello, world!'
sr5900p print --dry-run --printer ${PRINTER_IP} --qr-text 'Hello, world!' # width auto detect

# Detect your printer's IP with avahi-browse:
sudo apt-get install -y avahi-utils
PRINTER_IP=`avahi-browse -alrpt | grep -E '^=.*SR5900P' | cut -d ';' -f 8`

# OR, you can manually set the IP:
PRINTER_IP=${YOUR_PRINTER_IP}

# Let's print! (RUST_LOG=debug also shows the status read before printing)
sr5900p print --printer ${PRINTER_IP} --test-pattern
sr5900p print --printer ${PRINTER_IP} --qr-text 'Hello, world!'
```

## Label templates

A template (TOML) lays out text / QR code / image columns from left to right.
Text is rendered with TTF/OTF fonts, so Japanese text and multiple fonts and sizes are supported.
A label longer than the template's `max_length_mm` is rejected instead of printed.
See `src/label/template.rs` for the format and `templates/` for examples.

```
sr5900p print --dry-run --width 12 --template templates/shelf.toml \
  --field dept=開発部 --field shelf_id=SH-A03
```

From Rust:

```rust
let td = Template::load("templates/shelf.toml")?.render(&fields, Tape::W12)?;
sr5900p::label::save_preview("preview.png", &td)?;
sr5900p::printer::print_display(printer_ip, &td)?;
```

## License
MIT

## Author
hikalium (original author)

## Special Thanks
hikalium (for [the original implementation](https://github.com/hikalium/sr5900p) this fork is based on)
Mine02C4 (for [the initial analysis of the protocol](https://github.com/Mine02C4/TEPRA_PRO_SR5900P_analysis) )
