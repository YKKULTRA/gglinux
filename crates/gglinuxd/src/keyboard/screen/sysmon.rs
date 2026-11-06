// SPDX-License-Identifier: GPL-3.0-or-later
//! The System Monitor on the keyboard's screen: one to four readings of the computer,
//! a line each, drawn again once a second.
//!
//! Each reading comes from a file the kernel keeps: CPU use from two samples of
//! `/proc/stat` a second apart, memory from `/proc/meminfo`, the CPU's temperature from
//! hwmon (`coretemp`'s "Package id 0", or else `k10temp`'s first sensor). The graphics
//! card has its own rule, in [`super::gpu`]: it's never woken to ask. The roots are
//! handed in, so tests read made-up trees instead of this computer's.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gg_device_keyboard::{font, oled, Bitmap};
use gg_ipc::MonitorItem;

use super::gpu::{self, Gpu, Runner};
use super::{Now, ScreenSource};

/// Where the kernel's files are: `/proc` and `/sys` on a real computer.
#[derive(Debug, Clone)]
pub struct Roots {
    pub proc: PathBuf,
    pub sys: PathBuf,
}

impl Roots {
    pub fn system() -> Roots {
        Roots {
            proc: PathBuf::from("/proc"),
            sys: PathBuf::from("/sys"),
        }
    }
}

/// CPU time from the first line of `/proc/stat`, in ticks: busy and in all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuTimes {
    busy: u64,
    total: u64,
}

/// The readings, as they're drawn.
#[derive(Debug, Clone, Default, PartialEq)]
struct Readings {
    cpu: Option<u8>,
    cpu_celsius: Option<i32>,
    /// Used and in all, in KiB.
    memory: Option<(u64, u64)>,
    gpu: Option<gpu::Shown>,
    hour: u8,
    minute: u8,
}

pub struct Monitor {
    items: Vec<MonitorItem>,
    roots: Roots,
    gpu: Gpu,
    cpu_before: Option<CpuTimes>,
    /// The hwmon file with the CPU's temperature, once found.
    temp_input: Option<PathBuf>,
    /// The wall-clock second last drawn, and its picture.
    second: Option<u64>,
    picture: Option<Bitmap>,
}

impl Monitor {
    pub fn new(items: &[MonitorItem], roots: Roots, runner: Arc<dyn Runner>) -> Monitor {
        Monitor {
            items: items.to_vec(),
            gpu: Gpu::new(roots.sys.clone(), runner),
            roots,
            cpu_before: None,
            temp_input: None,
            second: None,
            picture: None,
        }
    }

    /// Read what the items need. Nothing is read for an item that isn't shown; in
    /// particular, the graphics card isn't looked at unless a GPU item is on.
    fn read(&mut self, now: &Now) -> Readings {
        let wants = |i: MonitorItem| self.items.contains(&i);
        let (cpu, temp, ram) = (
            wants(MonitorItem::Cpu),
            wants(MonitorItem::CpuTemp),
            wants(MonitorItem::Ram),
        );
        let gpu = wants(MonitorItem::Gpu) || wants(MonitorItem::GpuTemp);
        let mut r = Readings {
            hour: now.local.hour,
            minute: now.local.minute,
            ..Readings::default()
        };
        if cpu {
            let times = cpu_times(&self.roots.proc);
            r.cpu = self.cpu_before.zip(times).and_then(|(a, b)| cpu_use(a, b));
            self.cpu_before = times;
        }
        if temp {
            let mut m = self.temp_input.as_deref().and_then(millidegrees);
            if m.is_none() {
                // Not found yet, or gone: look again.
                self.temp_input = cpu_temp_input(&self.roots.sys);
                m = self.temp_input.as_deref().and_then(millidegrees);
            }
            r.cpu_celsius = m.map(|m| (m as f64 / 1000.0).round() as i32);
        }
        if ram {
            r.memory = memory(&self.roots.proc);
        }
        if gpu {
            r.gpu = Some(self.gpu.read(now.instant));
        }
        r
    }

    /// The readings drawn, at the size the widest possible line of these items allows.
    fn render(&self, readings: &Readings) -> Bitmap {
        let chars = self.items.iter().map(|&i| widest(i)).max().unwrap_or(0);
        draw(&lines(&self.items, readings), chars)
    }
}

impl ScreenSource for Monitor {
    /// Read and drawn once a wall-clock second; the same picture in between, however
    /// often the loop asks.
    fn frame(&mut self, now: &Now) -> Option<Bitmap> {
        let second = now.since_epoch.as_secs();
        if self.second != Some(second) || self.picture.is_none() {
            let readings = self.read(now);
            self.picture = Some(self.render(&readings));
            self.second = Some(second);
        }
        self.picture.clone()
    }

    /// Just after each second turns.
    fn next_due(&self, now: &Now) -> Option<Duration> {
        let into_second = now.since_epoch.subsec_millis();
        Some(Duration::from_millis(u64::from(1000 - into_second) + 5))
    }
}

/// Each item's line: a label on the left, its value on the right. "--" is a reading that
/// couldn't be had (yet).
fn lines(items: &[MonitorItem], r: &Readings) -> Vec<(&'static str, String)> {
    let or_dashes = |v: Option<String>, unit: &str| v.unwrap_or_else(|| format!("--{unit}"));
    let celsius = |c: Option<i32>| or_dashes(c.map(|c| format!("{c}°C")), "°C");
    let gpu_read = match r.gpu {
        Some(gpu::Shown::Read { busy, celsius }) => Some((busy, celsius)),
        _ => None,
    };
    items
        .iter()
        .map(|item| match item {
            MonitorItem::Cpu => ("CPU", or_dashes(r.cpu.map(|c| format!("{c}%")), "%")),
            MonitorItem::CpuTemp => ("CPU", celsius(r.cpu_celsius)),
            MonitorItem::Ram => (
                "RAM",
                or_dashes(r.memory.map(|(used, total)| gigabytes(used, total)), ""),
            ),
            MonitorItem::Gpu => (
                "GPU",
                match r.gpu {
                    Some(gpu::Shown::Idle) => "idle".to_owned(),
                    _ => or_dashes(gpu_read.and_then(|g| g.0).map(|b| format!("{b}%")), "%"),
                },
            ),
            MonitorItem::GpuTemp => ("GPU", celsius(gpu_read.and_then(|g| g.1))),
            MonitorItem::Clock => ("Time", format!("{:02}:{:02}", r.hour, r.minute)),
        })
        .collect()
}

/// "9.1/62 GB": used with one decimal, all of it whole from 10 GB up (GiB, as the
/// desktop's system monitors count them).
fn gigabytes(used_kib: u64, total_kib: u64) -> String {
    const KIB_PER_GIB: f64 = 1024.0 * 1024.0;
    let (used, total) = (
        used_kib as f64 / KIB_PER_GIB,
        total_kib as f64 / KIB_PER_GIB,
    );
    if total >= 10.0 {
        format!("{used:.1}/{total:.0} GB")
    } else {
        format!("{used:.1}/{total:.1} GB")
    }
}

/// The widest each item's line usually gets, in characters, so the size doesn't jump as
/// the readings change.
fn widest(item: MonitorItem) -> usize {
    match item {
        MonitorItem::Cpu | MonitorItem::Gpu => "CPU 100%".len(),
        MonitorItem::CpuTemp | MonitorItem::GpuTemp => "CPU 99°C".chars().count(),
        MonitorItem::Ram => "RAM 99.9/999 GB".len(),
        MonitorItem::Clock => "Time 00:00".len(),
    }
}

/// The lines stacked and centred as a block, each label on the block's left and its value
/// on its right. The block is `chars` characters wide (more if a line is longer), drawn
/// as big as that lets it be.
fn draw(lines: &[(&'static str, String)], chars: usize) -> Bitmap {
    const GAP: usize = 2;
    let chars = lines
        .iter()
        .map(|(label, value)| label.len() + 1 + value.chars().count())
        .fold(chars, usize::max);
    let scale = scale_for(lines.len(), chars);
    let width = chars * 8 * scale;
    let height = lines.len() * 8 * scale + GAP * lines.len().saturating_sub(1);
    let left = oled::WIDTH.saturating_sub(width) / 2;
    let mut y = oled::HEIGHT.saturating_sub(height) / 2;
    let mut b = Bitmap::default();
    for (label, value) in lines {
        put(&mut b, left, y, label, scale);
        let right = left + width;
        let value_width = value.chars().count() * 8 * scale;
        put(&mut b, right.saturating_sub(value_width), y, value, scale);
        y += 8 * scale + GAP;
    }
    b
}

/// The biggest size, 1 to 3, at which `count` lines of `chars` characters fit.
fn scale_for(count: usize, chars: usize) -> usize {
    (1..=3)
        .rev()
        .find(|&s| {
            chars * 8 * s <= oled::WIDTH
                && count * 8 * s + 2 * count.saturating_sub(1) <= oled::HEIGHT
        })
        .unwrap_or(1)
}

/// The font has no degree sign: this one sits in the top left of its cell.
const DEGREE: [u8; 8] = [0x06, 0x09, 0x09, 0x06, 0, 0, 0, 0];

/// `text` at `(x, y)` like [`Bitmap::draw_text`], with a degree sign of its own.
fn put(b: &mut Bitmap, x: usize, y: usize, text: &str, scale: usize) {
    for (i, c) in text.chars().enumerate() {
        let glyph = if c == '°' { &DEGREE } else { font::glyph(c) };
        let gx = x + i * 8 * scale;
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            b.set(gx + col * scale + dx, y + row * scale + dy, true);
                        }
                    }
                }
            }
        }
    }
}

/// The first line of `<proc>/stat`: user nice system idle iowait irq softirq steal.
fn cpu_times(proc: &Path) -> Option<CpuTimes> {
    let stat = std::fs::read_to_string(proc.join("stat")).ok()?;
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let ticks: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|t| t.parse().ok())
        .collect::<Option<_>>()?;
    let total: u64 = ticks.iter().sum();
    let idle = ticks.get(3)? + ticks.get(4).copied().unwrap_or(0);
    Some(CpuTimes {
        busy: total.saturating_sub(idle),
        total,
    })
}

/// The share of CPU time spent busy between two samples, in %.
fn cpu_use(before: CpuTimes, after: CpuTimes) -> Option<u8> {
    let total = after.total.checked_sub(before.total)?;
    let busy = after.busy.checked_sub(before.busy)?;
    (total > 0).then(|| ((busy * 100 + total / 2) / total).min(100) as u8)
}

/// Memory in use (all of it less what's available) and in all, in KiB.
fn memory(proc: &Path) -> Option<(u64, u64)> {
    let info = std::fs::read_to_string(proc.join("meminfo")).ok()?;
    let field = |name: &str| -> Option<u64> {
        info.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    let total = field("MemTotal")?;
    let available = field("MemAvailable")?;
    Some((total.saturating_sub(available), total))
}

/// The file with the CPU package's temperature: `coretemp`'s "Package id 0", or else the
/// first sensor of `k10temp`. Only each hwmon's `name` is read before that.
fn cpu_temp_input(sys: &Path) -> Option<PathBuf> {
    let mut hwmons: Vec<PathBuf> = std::fs::read_dir(sys.join("class/hwmon"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    // hwmon2 before hwmon10.
    hwmons.sort_by_key(|p| {
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        (name.len(), name.into_owned())
    });
    let named = |want: &'static str| -> Vec<&PathBuf> {
        hwmons
            .iter()
            .filter(|d| std::fs::read_to_string(d.join("name")).is_ok_and(|n| n.trim() == want))
            .collect()
    };
    for dir in named("coretemp") {
        let Ok(files) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut labels: Vec<PathBuf> = files
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("temp") && n.ends_with("_label"))
            })
            .collect();
        labels.sort();
        for label in labels {
            if std::fs::read_to_string(&label).is_ok_and(|l| l.trim() == "Package id 0") {
                let name = label.file_name()?.to_str()?.replace("_label", "_input");
                return Some(label.with_file_name(name));
            }
        }
    }
    named("k10temp")
        .into_iter()
        .map(|d| d.join("temp1_input"))
        .find(|p| p.exists())
}

fn millidegrees(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::screen::gpu::tests::{device, Fake, Never};
    use crate::keyboard::screen::LocalTime;
    use crate::scratch::Scratch;
    use std::time::Instant;

    fn write(path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn stat(proc: &Path, user: u64, idle: u64) {
        write(
            proc.join("stat"),
            &format!("cpu  {user} 0 0 {idle} 0 0 0 0 0 0\ncpu0 1 2 3 4 5 6 7 8 9 10\nintr 1 2\n"),
        );
    }

    fn roots(d: &Scratch) -> Roots {
        Roots {
            proc: d.0.join("proc"),
            sys: d.0.join("sys"),
        }
    }

    fn at(second: u64, hour: u8, minute: u8) -> Now {
        Now {
            since_epoch: Duration::from_secs(second),
            local: LocalTime {
                hour,
                minute,
                second: 0,
                weekday: 0,
                day: 1,
                month: 0,
            },
            instant: Instant::now(),
        }
    }

    const MEMINFO: &str = "MemTotal:       65011712 kB\nMemFree:         1000000 kB\n\
                           MemAvailable:   55469670 kB\nBuffers:          100000 kB\n";

    #[test]
    fn cpu_use_comes_from_two_samples_of_proc_stat() {
        let d = Scratch::new("mon-cpu");
        let proc = d.0.join("proc");
        stat(&proc, 1000, 9000);
        let before = cpu_times(&proc).unwrap();
        assert_eq!(
            before,
            CpuTimes {
                busy: 1000,
                total: 10_000
            }
        );
        // 25 busy of 100 ticks since.
        stat(&proc, 1025, 9075);
        assert_eq!(cpu_use(before, cpu_times(&proc).unwrap()), Some(25));
        assert_eq!(cpu_use(before, before), None, "no time passed");
        // iowait counts as idle, steal as busy.
        write(proc.join("stat"), "cpu  10 0 0 70 20 0 0 0\n");
        assert_eq!(cpu_times(&proc).unwrap().busy, 10);
        write(proc.join("stat"), "cpu  oops\n");
        assert_eq!(cpu_times(&proc), None);
    }

    #[test]
    fn memory_in_use_is_all_of_it_less_what_s_available() {
        let d = Scratch::new("mon-ram");
        write(d.0.join("proc/meminfo"), MEMINFO);
        let (used, total) = memory(&d.0.join("proc")).unwrap();
        assert_eq!((used, total), (9_542_042, 65_011_712));
        assert_eq!(gigabytes(used, total), "9.1/62 GB");
        assert_eq!(gigabytes(1_258_291, 8_178_892), "1.2/7.8 GB");
        assert_eq!(memory(&d.0.join("nowhere")), None);
    }

    #[test]
    fn the_cpu_temperature_is_coretemp_s_package_or_else_k10temp_s_first() {
        let d = Scratch::new("mon-temp");
        let sys = d.0.join("sys");
        let hw = sys.join("class/hwmon");
        // Another chip first, and core sensors besides the package.
        write(hw.join("hwmon0/name"), "nvme\n");
        write(hw.join("hwmon0/temp1_input"), "99000\n");
        write(hw.join("hwmon3/name"), "coretemp\n");
        write(hw.join("hwmon3/temp2_label"), "Core 0\n");
        write(hw.join("hwmon3/temp2_input"), "50000\n");
        write(hw.join("hwmon3/temp1_label"), "Package id 0\n");
        write(hw.join("hwmon3/temp1_input"), "54200\n");
        let input = cpu_temp_input(&sys).unwrap();
        assert_eq!(input, hw.join("hwmon3/temp1_input"));
        assert_eq!(millidegrees(&input), Some(54_200));

        let amd = Scratch::new("mon-k10");
        let hw = amd.0.join("sys/class/hwmon");
        write(hw.join("hwmon10/name"), "k10temp\n");
        write(hw.join("hwmon10/temp1_input"), "61000\n");
        write(hw.join("hwmon2/name"), "k10temp\n");
        write(hw.join("hwmon2/temp1_input"), "48000\n");
        assert_eq!(
            cpu_temp_input(&amd.0.join("sys")),
            Some(hw.join("hwmon2/temp1_input")),
            "hwmon2 comes before hwmon10"
        );

        let none = Scratch::new("mon-notemp");
        write(none.0.join("sys/class/hwmon/hwmon0/name"), "acpitz\n");
        assert_eq!(cpu_temp_input(&none.0.join("sys")), None);
    }

    #[test]
    fn each_item_reads_as_a_label_and_a_value() {
        let items = [
            MonitorItem::Cpu,
            MonitorItem::CpuTemp,
            MonitorItem::Ram,
            MonitorItem::Gpu,
            MonitorItem::GpuTemp,
            MonitorItem::Clock,
        ];
        let r = Readings {
            cpu: Some(12),
            cpu_celsius: Some(54),
            memory: Some((9_542_042, 65_011_712)),
            gpu: Some(gpu::Shown::Read {
                busy: Some(3),
                celsius: Some(41),
            }),
            hour: 9,
            minute: 5,
        };
        let shown = |r: &Readings| -> Vec<String> {
            lines(&items, r)
                .into_iter()
                .map(|(l, v)| format!("{l} {v}"))
                .collect()
        };
        assert_eq!(
            shown(&r),
            [
                "CPU 12%",
                "CPU 54°C",
                "RAM 9.1/62 GB",
                "GPU 3%",
                "GPU 41°C",
                "Time 09:05"
            ]
        );
        let asleep = Readings {
            gpu: Some(gpu::Shown::Idle),
            ..Readings::default()
        };
        assert_eq!(
            shown(&asleep),
            [
                "CPU --%",
                "CPU --°C",
                "RAM --",
                "GPU idle",
                "GPU --°C",
                "Time 00:00"
            ]
        );
        let unknown = Readings {
            gpu: Some(gpu::Shown::Unknown),
            ..Readings::default()
        };
        assert_eq!(shown(&unknown)[3], "GPU --%");
    }

    /// The rows each line of text covers, top to bottom.
    fn bands(b: &Bitmap) -> Vec<(usize, usize)> {
        let lit: Vec<bool> = (0..oled::HEIGHT)
            .map(|y| (0..oled::WIDTH).any(|x| b.get(x, y)))
            .collect();
        let mut bands = Vec::new();
        let mut y = 0;
        while y < lit.len() {
            if lit[y] {
                let top = y;
                while y < lit.len() && lit[y] {
                    y += 1;
                }
                bands.push((top, y - 1));
            }
            y += 1;
        }
        bands
    }

    #[test]
    fn three_readings_are_three_lines_with_their_values_lined_up_on_the_right() {
        let three = vec![
            ("CPU", "12%".to_owned()),
            ("RAM", "9.1/62 GB".to_owned()),
            ("Time", "13:05".to_owned()),
        ];
        let b = draw(&three, widest(MonitorItem::Ram));
        assert_eq!(bands(&b).len(), 3, "{b:?}");
        let right_edge = |rows: (usize, usize)| {
            (0..oled::WIDTH)
                .rev()
                .find(|&x| (rows.0..=rows.1).any(|y| b.get(x, y)))
                .unwrap()
        };
        let edges: Vec<usize> = bands(&b).into_iter().map(right_edge).collect();
        assert!(
            edges.iter().all(|e| edges[0].abs_diff(*e) <= 1),
            "values end together: {edges:?}"
        );
        // One short reading is drawn bigger: 8 characters at twice the size.
        let one = draw(&[("CPU", "7%".to_owned())], widest(MonitorItem::Cpu));
        let rows = bands(&one);
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].1 - rows[0].0 >= 12,
            "twice the 8-pixel font: {one:?}"
        );
        // Four lines of the widest kind still fit.
        let four = draw(
            &[
                ("RAM", "99.9/999 GB".to_owned()),
                ("CPU", "100°C".to_owned()),
                ("GPU", "100%".to_owned()),
                ("Time", "23:59".to_owned()),
            ],
            widest(MonitorItem::Ram),
        );
        assert_eq!(bands(&four).len(), 4, "{four:?}");
    }

    #[test]
    fn the_degree_sign_is_drawn_not_a_question_mark() {
        let with = draw(&[("CPU", "54°C".to_owned())], 8);
        let question = draw(&[("CPU", "54?C".to_owned())], 8);
        assert_ne!(with, question);
    }

    #[test]
    fn the_monitor_reads_once_a_second_however_often_it_is_asked() {
        let d = Scratch::new("mon-once");
        let r = roots(&d);
        stat(&r.proc, 1000, 9000);
        write(r.proc.join("meminfo"), MEMINFO);
        let mut m = Monitor::new(
            &[MonitorItem::Cpu, MonitorItem::Ram, MonitorItem::Clock],
            r.clone(),
            Arc::new(Never),
        );
        let first = m.frame(&at(100, 13, 5)).unwrap();
        assert_eq!(bands(&first).len(), 3);
        // Within the same second, nothing is read again.
        stat(&r.proc, 1050, 9050);
        assert_eq!(m.frame(&at(100, 13, 5)).unwrap(), first);
        // The next second has the CPU's use since the first sample: 50 of 100 ticks.
        let next = m.frame(&at(101, 13, 5)).unwrap();
        let expected = Readings {
            cpu: Some(50),
            memory: memory(&r.proc),
            hour: 13,
            minute: 5,
            ..Readings::default()
        };
        assert_eq!(next, m.render(&expected));
        assert_ne!(next, first, "the first second had no CPU use yet");
        let half = Now {
            since_epoch: Duration::from_millis(101_400),
            ..at(101, 13, 5)
        };
        assert_eq!(m.next_due(&half), Some(Duration::from_millis(605)));
    }

    #[test]
    fn a_monitor_without_gpu_items_never_looks_at_the_card_and_with_a_sleeping_card_never_asks_it()
    {
        let d = Scratch::new("mon-gpu");
        let r = roots(&d);
        stat(&r.proc, 1, 1);
        device(
            &r.sys,
            "0000:01:00.0",
            "0x10de",
            "0x030000",
            "suspended",
            "nvidia",
        );
        let mut m = Monitor::new(
            &[MonitorItem::Gpu, MonitorItem::GpuTemp],
            r.clone(),
            Arc::new(Never),
        );
        for s in 0..10 {
            m.frame(&at(s, 0, 0));
        }
        assert_eq!(
            m.frame(&at(10, 0, 0)).unwrap(),
            draw(
                &[("GPU", "idle".to_owned()), ("GPU", "--°C".to_owned())],
                widest(MonitorItem::GpuTemp)
            )
        );

        // Awake, but the monitor shows only the CPU: the card is not asked.
        crate::keyboard::screen::gpu::tests::set_status(&r.sys, "0000:01:00.0", "active");
        let fake = Fake::answering("10, 50");
        let mut cpu_only = Monitor::new(&[MonitorItem::Cpu], r, fake.clone());
        for s in 0..10 {
            cpu_only.frame(&at(s, 0, 0));
        }
        assert_eq!(fake.count(), 0);
    }
}
