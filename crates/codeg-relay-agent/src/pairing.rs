use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
pub struct Pairing {
    pub v: u8,
    pub mode: &'static str,
    pub url: String,
    pub token: String,
    pub fp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

pub fn create(
    config: &crate::config::Config,
    state_dir: &Path,
    name: Option<String>,
) -> anyhow::Result<Pairing> {
    let identity = crate::certificate::load(state_dir, false)
        .map_err(|_| anyhow::anyhow!("cannot load home identity"))?;
    Ok(Pairing {
        v: 1,
        mode: "relay",
        url: config.effective_public_url().into(),
        token: crate::token::read_source(&config.token_source)?,
        fp: identity.fingerprint,
        name,
    })
}
fn qr(payload: &Pairing) -> anyhow::Result<qrcode::QrCode> {
    qrcode::QrCode::with_error_correction_level(serde_json::to_vec(payload)?, qrcode::EcLevel::M)
        .map_err(|_| anyhow::anyhow!("pairing is too large for a QR code; use --json-only"))
}
pub fn write_png(payload: &Pairing, path: &Path) -> anyhow::Result<()> {
    let qr = qr(payload)?;
    let modules = qr.width() + 8;
    let scale = 8;
    let width = modules * scale;
    let mut pixels = vec![255; width * width];
    for y in 0..qr.width() {
        for x in 0..qr.width() {
            if qr[(x, y)] == qrcode::Color::Dark {
                for dy in 0..scale {
                    let offset = ((y + 4) * scale + dy) * width + (x + 4) * scale;
                    pixels[offset..offset + scale].fill(0);
                }
            }
        }
    }
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, width as u32, width as u32);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&pixels)?;
    }
    crate::secrets::write_private(path, &encoded, false)
}
pub fn terminal_qr(payload: &Pairing) -> anyhow::Result<String> {
    let qr = qr(payload)?;
    let mut result = String::new();
    for y in 0..qr.width() + 8 {
        for x in 0..qr.width() + 8 {
            let dark = x >= 4
                && y >= 4
                && x < qr.width() + 4
                && y < qr.width() + 4
                && qr[(x - 4, y - 4)] == qrcode::Color::Dark;
            result.push_str(if dark { "\x1b[40m  " } else { "\x1b[47m  " });
        }
        result.push_str("\x1b[0m\n");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pairing_schema_and_qr_png_are_private_and_usable() {
        let payload = Pairing {
            v: 1,
            mode: "relay",
            url: "https://relay.example:18443".into(),
            token: "test-only-value".into(),
            fp: "ab".repeat(32),
            name: Some("Home".into()),
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["v"], 1);
        assert_eq!(value["mode"], "relay");
        assert_eq!(value["fp"], "ab".repeat(32));
        let qr = terminal_qr(&payload).unwrap();
        assert!(qr.contains("\x1b[40m"));
        assert!(qr.contains("\x1b[47m"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pair.png");
        write_png(&payload, &path).unwrap();
        let decoder = png::Decoder::new(std::fs::File::open(&path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut bytes = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut bytes).unwrap();
        assert_eq!(info.width, info.height);
        assert!(info.width > 200);
        assert!(bytes.contains(&0));
        assert!(bytes.contains(&255));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
