use std::{thread, time::Duration};

use anyhow::{Context, Result, bail};
use clap::Parser;
use hidapi::{DeviceInfo, HidApi, HidDevice};

const RAZER_VID: u16 = 0x1532;
const REPORT_PAYLOAD_LEN: usize = 90;
const HIDAPI_REPORT_LEN: usize = REPORT_PAYLOAD_LEN + 1;

#[derive(Debug, Parser)]
#[command(about = "Set DeathAdder V2 and DeathAdder V3 Pro polling rate and DPI")]
struct Args {
    /// Polling rate in Hz (up to 1000 on V2 or 8000 on V3 Pro wireless).
    #[arg(value_parser = clap::value_parser!(u16))]
    rate: Option<u16>,

    /// Set equal X/Y DPI.
    #[arg(long, value_parser = clap::value_parser!(u16))]
    dpi: Option<u16>,

    /// Print matching Razer HID interfaces and exit.
    #[arg(long)]
    list: bool,

    /// Print the current DPI and polling rate.
    #[arg(long)]
    status: bool,

    /// Restrict operations to product ID 00c3 (V3 Pro wireless) or 0084 (V2).
    #[arg(long, value_parser = parse_hex_u16)]
    pid: Option<u16>,

    /// Override the HID interface number. Interface 0 is used by the Razer protocol.
    #[arg(long, default_value_t = 0)]
    interface: i32,
}

#[derive(Clone, Copy)]
struct DeviceProfile {
    name: &'static str,
    transaction_id: u8,
    max_dpi: u16,
    high_rate_polling: bool,
}

fn parse_hex_u16(s: &str) -> std::result::Result<u16, String> {
    let s = s.trim_start_matches("0x");
    u16::from_str_radix(s, 16).map_err(|e| e.to_string())
}

fn device_profile(pid: u16) -> Result<DeviceProfile> {
    match pid {
        0x0084 => Ok(DeviceProfile {
            name: "DeathAdder V2",
            transaction_id: 0x3f,
            max_dpi: 20_000,
            high_rate_polling: false,
        }),
        0x00c3 => Ok(DeviceProfile {
            name: "DeathAdder V3 Pro wireless",
            transaction_id: 0x1f,
            max_dpi: 35_000,
            high_rate_polling: true,
        }),
        _ => bail!("unsupported Razer product ID {pid:04x}; use 0084 or 00c3"),
    }
}

fn target_pids(requested: Option<u16>, detected: impl IntoIterator<Item = u16>) -> Vec<u16> {
    if let Some(pid) = requested {
        return vec![pid];
    }

    let mut pids = Vec::new();
    for pid in detected {
        if device_profile(pid).is_ok() && !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    pids
}

fn rate_code(profile: DeviceProfile, rate: u16) -> Result<u8> {
    if profile.high_rate_polling {
        return match rate {
            8000 => Ok(0x01),
            4000 => Ok(0x02),
            2000 => Ok(0x04),
            1000 => Ok(0x08),
            500 => Ok(0x10),
            250 => Ok(0x20),
            125 => Ok(0x40),
            _ => bail!("unsupported rate {rate}; use 125, 250, 500, 1000, 2000, 4000, or 8000"),
        };
    }

    match rate {
        1000 => Ok(0x01),
        500 => Ok(0x02),
        125 => Ok(0x08),
        _ => bail!(
            "unsupported rate {rate} for {}; use 125, 500, or 1000",
            profile.name
        ),
    }
}

/// Build the 90-byte Razer feature report.
///
/// Layout:
/// status, transaction ID, remaining packets (BE), protocol, data size,
/// command class, command ID, 80 argument bytes, CRC, reserved.
fn build_report(
    transaction_id: u8,
    command_class: u8,
    command: u8,
    arguments: &[u8],
) -> [u8; REPORT_PAYLOAD_LEN] {
    let mut report = [0u8; REPORT_PAYLOAD_LEN];

    report[0] = 0x00; // new command
    report[1] = transaction_id;
    report[2] = 0x00; // remaining packets, high
    report[3] = 0x00; // remaining packets, low
    report[4] = 0x00; // protocol type
    report[5] = arguments.len() as u8;
    report[6] = command_class;
    report[7] = command;
    report[8..8 + arguments.len()].copy_from_slice(arguments);

    // OpenRazer XORs payload bytes 2 through 87 inclusive.
    report[88] = report[2..88].iter().fold(0u8, |crc, byte| crc ^ byte);
    report[89] = 0x00;

    report
}

fn build_poll_report(profile: DeviceProfile, rate: u16) -> Result<[u8; REPORT_PAYLOAD_LEN]> {
    let code = rate_code(profile, rate)?;
    if profile.high_rate_polling {
        Ok(build_report(
            profile.transaction_id,
            0x00,
            0x40,
            &[0x00, code],
        ))
    } else {
        Ok(build_report(profile.transaction_id, 0x00, 0x05, &[code]))
    }
}

fn build_dpi_report(profile: DeviceProfile, dpi: u16) -> Result<[u8; REPORT_PAYLOAD_LEN]> {
    if !(100..=profile.max_dpi).contains(&dpi) {
        bail!(
            "unsupported DPI {dpi} for {}; use 100 through {}",
            profile.name,
            profile.max_dpi
        );
    }

    let [high, low] = dpi.to_be_bytes();
    Ok(build_report(
        profile.transaction_id,
        0x04,
        0x05,
        &[0x01, high, low, high, low, 0x00, 0x00],
    ))
}

fn build_get_poll_report(profile: DeviceProfile) -> [u8; REPORT_PAYLOAD_LEN] {
    let command = if profile.high_rate_polling {
        0xc0
    } else {
        0x85
    };
    build_report(profile.transaction_id, 0x00, command, &[0x00])
}

fn build_get_dpi_report(profile: DeviceProfile) -> [u8; REPORT_PAYLOAD_LEN] {
    build_report(
        profile.transaction_id,
        0x04,
        0x85,
        &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    )
}

fn decode_poll_rate(profile: DeviceProfile, response: &[u8; REPORT_PAYLOAD_LEN]) -> Result<u16> {
    let code = response[if profile.high_rate_polling { 9 } else { 8 }];
    if profile.high_rate_polling {
        return match code {
            0x01 => Ok(8000),
            0x02 => Ok(4000),
            0x04 => Ok(2000),
            0x08 => Ok(1000),
            0x10 => Ok(500),
            0x20 => Ok(250),
            0x40 => Ok(125),
            _ => bail!("device returned unknown polling-rate code 0x{code:02x}"),
        };
    }

    match code {
        0x01 => Ok(1000),
        0x02 => Ok(500),
        0x08 => Ok(125),
        code => bail!("device returned unknown polling-rate code 0x{code:02x}"),
    }
}

fn decode_dpi(response: &[u8; REPORT_PAYLOAD_LEN]) -> (u16, u16) {
    (
        u16::from_be_bytes([response[9], response[10]]),
        u16::from_be_bytes([response[11], response[12]]),
    )
}

fn matching_device(api: &HidApi, pid: u16, interface: i32) -> Result<&DeviceInfo> {
    api.device_list()
        .find(|info| {
            info.vendor_id() == RAZER_VID
                && info.product_id() == pid
                && info.interface_number() == interface
        })
        .with_context(|| {
            format!(
                "Razer HID interface not found: {:04x}:{:04x}, interface {}.\n\
                 Run with --list and confirm the receiver is connected wirelessly.",
                RAZER_VID, pid, interface
            )
        })
}

fn print_devices(api: &HidApi) {
    for info in api.device_list().filter(|d| d.vendor_id() == RAZER_VID) {
        println!(
            "{:04x}:{:04x} interface={} usage_page=0x{:04x} usage=0x{:04x} product={:?} path={}",
            info.vendor_id(),
            info.product_id(),
            info.interface_number(),
            info.usage_page(),
            info.usage(),
            info.product_string(),
            info.path().to_string_lossy(),
        );
    }
}

fn send_once(
    device: &HidDevice,
    payload: &[u8; REPORT_PAYLOAD_LEN],
) -> Result<[u8; REPORT_PAYLOAD_LEN]> {
    // hidapi reserves byte zero for the HID report ID. Razer uses report ID 0,
    // followed by the actual 90-byte feature-report payload.
    let mut feature = [0u8; HIDAPI_REPORT_LEN];
    feature[0] = 0x00;
    feature[1..].copy_from_slice(payload);

    device
        .send_feature_report(&feature)
        .context("SET_REPORT failed; check hidraw permissions and stop OpenRazer first")?;

    thread::sleep(Duration::from_millis(50));

    let mut response = [0u8; HIDAPI_REPORT_LEN];
    response[0] = 0x00;
    let read = device
        .get_feature_report(&mut response)
        .context("GET_REPORT failed")?;

    // Account for hidapi retaining or omitting the report-ID byte depending on backend.
    let offset = usize::from(read >= HIDAPI_REPORT_LEN);
    let response_len = read.saturating_sub(offset);
    if response_len < 9 {
        bail!("short feature report response: read {read} bytes");
    }

    let mut report = [0u8; REPORT_PAYLOAD_LEN];
    let copy_len = response_len.min(REPORT_PAYLOAD_LEN);
    report[..copy_len].copy_from_slice(&response[offset..offset + copy_len]);

    let status = report[0];
    let class = report[6];
    let command = report[7];

    if class != payload[6] || command != payload[7] {
        bail!(
            "unexpected response: status=0x{status:02x}, class=0x{class:02x}, \
             command=0x{command:02x}"
        );
    }

    // Razer status 0x02 is success; 0x01 is busy but OpenRazer treats it as success.
    if status != 0x02 && status != 0x01 {
        bail!("Razer command failed with status 0x{status:02x}");
    }

    Ok(report)
}

fn send_report(
    device: &HidDevice,
    payload: &[u8; REPORT_PAYLOAD_LEN],
) -> Result<[u8; REPORT_PAYLOAD_LEN]> {
    let mut last_error = None;

    for _ in 0..5 {
        match send_once(device, payload) {
            Ok(response) => return Ok(response),
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    Err(last_error.expect("the retry loop runs at least once"))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let api = HidApi::new().context("failed to initialize hidapi")?;

    if args.list {
        print_devices(&api);
        return Ok(());
    }

    if args.rate.is_none() && args.dpi.is_none() && !args.status {
        bail!("provide a polling rate, --dpi, --status, or --list");
    }

    let pids = target_pids(
        args.pid,
        api.device_list()
            .filter(|info| {
                info.vendor_id() == RAZER_VID && info.interface_number() == args.interface
            })
            .map(DeviceInfo::product_id),
    );
    if pids.is_empty() {
        bail!("no supported Razer mice found; run with --list");
    }

    // Validate every requested write before changing any device.
    for &pid in &pids {
        let profile = device_profile(pid)?;
        if let Some(rate) = args.rate {
            let _ = build_poll_report(profile, rate)?;
        }
        if let Some(dpi) = args.dpi {
            let _ = build_dpi_report(profile, dpi)?;
        }
    }

    for pid in pids {
        let profile = device_profile(pid)?;
        let info = matching_device(&api, pid, args.interface)?;
        println!(
            "Opening {:04x}:{:04x}, interface {}, path {}",
            info.vendor_id(),
            info.product_id(),
            info.interface_number(),
            info.path().to_string_lossy()
        );

        let device = info
            .open_device(&api)
            .context("failed to open hidraw device; check udev permissions")?;

        if let Some(rate) = args.rate {
            let report = build_poll_report(profile, rate)?;
            let _ = send_report(&device, &report)?;
            println!("{}: polling rate set to {rate} Hz", profile.name);
        }

        if let Some(dpi) = args.dpi {
            let report = build_dpi_report(profile, dpi)?;
            let _ = send_report(&device, &report)?;
            println!("{}: DPI set to {dpi}", profile.name);
        }

        if args.status {
            let poll_response = send_report(&device, &build_get_poll_report(profile))?;
            let dpi_response = send_report(&device, &build_get_dpi_report(profile))?;
            let rate = decode_poll_rate(profile, &poll_response)?;
            let (dpi_x, dpi_y) = decode_dpi(&dpi_response);
            println!(
                "{}: DPI {dpi_x}x{dpi_y}, polling rate {rate} Hz",
                profile.name
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deathadder_v3_pro_8k_polling_report_matches_openrazer() {
        let report = build_poll_report(device_profile(0x00c3).unwrap(), 8000).unwrap();

        assert_eq!(
            &report[0..10],
            &[0x00, 0x1f, 0, 0, 0, 2, 0x00, 0x40, 0x00, 0x01]
        );
        assert_eq!(
            report[88],
            report[2..88].iter().fold(0, |crc, byte| crc ^ byte)
        );
    }

    #[test]
    fn deathadder_v2_dpi_report_matches_openrazer() {
        let report = build_dpi_report(device_profile(0x0084).unwrap(), 1600).unwrap();

        assert_eq!(
            &report[0..15],
            &[
                0x00, 0x3f, 0, 0, 0, 7, 0x04, 0x05, 0x01, 0x06, 0x40, 0x06, 0x40, 0, 0
            ]
        );
        assert_eq!(
            report[88],
            report[2..88].iter().fold(0, |crc, byte| crc ^ byte)
        );
        assert!(build_poll_report(device_profile(0x0084).unwrap(), 8000).is_err());
    }

    #[test]
    fn unsupported_device_is_rejected() {
        assert!(device_profile(0x00c1).is_err());
    }

    #[test]
    fn status_reports_and_responses_match_openrazer() {
        let profile = device_profile(0x00c3).unwrap();
        let poll_request = build_get_poll_report(profile);
        let dpi_request = build_get_dpi_report(profile);
        assert_eq!(&poll_request[0..8], &[0x00, 0x1f, 0, 0, 0, 1, 0x00, 0xc0]);
        assert_eq!(
            &dpi_request[0..9],
            &[0x00, 0x1f, 0, 0, 0, 7, 0x04, 0x85, 0x00]
        );

        let mut poll_response = [0u8; REPORT_PAYLOAD_LEN];
        poll_response[9] = 0x01;
        assert_eq!(decode_poll_rate(profile, &poll_response).unwrap(), 8000);

        let mut dpi_response = [0u8; REPORT_PAYLOAD_LEN];
        dpi_response[9..13].copy_from_slice(&[0x06, 0x40, 0x03, 0x20]);
        assert_eq!(decode_dpi(&dpi_response), (1600, 800));
    }

    #[test]
    fn no_pid_filter_targets_each_supported_mouse_once() {
        let detected = [0x0084, 0x0084, 0x057d, 0x00c3, 0x00c3];

        assert_eq!(target_pids(None, detected), vec![0x0084, 0x00c3]);
        assert_eq!(target_pids(Some(0x00c3), detected), vec![0x00c3]);
    }
}
