use iced::{Element, Font, Theme};
use std::{fs, io::BufWriter, time::Duration};
use vime::{
    config::HotkeyMode,
    overlay::{view_snapshot, Message, UiPhase, UiSnapshot},
};

const MIXED: &str =
    "你好，世界。实时字幕让表达更轻松。繁體中文測試。\nRust + Iced · streaming transcription 123";

fn font() -> Font {
    Font::with_name(if cfg!(windows) {
        "Microsoft YaHei UI"
    } else {
        "Noto Sans CJK SC"
    })
}
fn view(snapshot: &UiSnapshot) -> Element<'_, Message> {
    view_snapshot(snapshot, font(), "Alt+F9", HotkeyMode::PushToTalk)
}

#[test]
fn installed_font_covers_chinese_and_english() {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let family = if cfg!(windows) {
        "Microsoft YaHei UI"
    } else {
        "Noto Sans CJK SC"
    };
    let id = db
        .query(&fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            ..Default::default()
        })
        .unwrap_or_else(|| panic!("Install {family} before running overlay rendering tests"));
    db.with_face_data(id, |data, index| {
        let face = ttf_parser::Face::parse(data, index).expect("valid installed font");
        for character in MIXED.chars().filter(|c| !c.is_whitespace()) {
            assert!(
                face.glyph_index(character).is_some(),
                "{family} has no glyph for {character}"
            );
        }
    })
    .expect("font face is readable");
}

#[test]
fn screenshots_cover_statuses_scales_and_long_transcripts() {
    let directory = std::env::var_os("VIME_SCREENSHOT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "target/overlay-screenshots".into());
    fs::create_dir_all(&directory).unwrap();
    let cases = [
        (
            "connecting",
            UiPhase::Connecting,
            "".to_owned(),
            "正在连接识别服务…",
        ),
        ("recording", UiPhase::Recording, MIXED.to_owned(), ""),
        (
            "three-lines",
            UiPhase::Recording,
            "第一行 · First line\n第二行 · Second line\n第三行 · Third line".to_owned(),
            "",
        ),
        (
            "long-transcript",
            UiPhase::Recording,
            format!("{}\n最新一句 · newest sentence", MIXED.repeat(16)),
            "",
        ),
        (
            "finalizing",
            UiPhase::Finalizing,
            MIXED.to_owned(),
            "正在完成识别…",
        ),
        (
            "delivered",
            UiPhase::Delivered,
            MIXED.to_owned(),
            "已复制到剪贴板",
        ),
        (
            "recovered",
            UiPhase::Recovered,
            MIXED.to_owned(),
            "识别中断 · 已恢复文字并复制",
        ),
        (
            "error",
            UiPhase::Error,
            String::new(),
            "麦克风不可用 · 请检查音频设置",
        ),
    ];
    let mut html = String::from("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>vime overlay review</title><style>body{font:16px system-ui;background:#e6e9ed;padding:24px}figure{margin:24px 0}img{width:480px;max-width:100%;height:auto}figcaption{margin:8px 0}</style><h1>vime overlay review</h1><p>Logical size 480 × 190. Each status is rendered at 100%, 125%, 150% and 200%. Review mixed-language glyphs, clipping and the newest line in the long transcript.</p>");
    for (name, phase, text, detail) in cases {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let snapshot = UiSnapshot {
                phase,
                text: text.clone(),
                detail: detail.into(),
                started: None,
            };
            let app = iced::application(
                move || snapshot.clone(),
                |_: &mut UiSnapshot, _: Message| {},
                view,
            )
            .default_font(font());
            let image = iced_test::screenshot(
                &app,
                &Theme::Dark,
                iced::Size::new(480.0, 190.0),
                scale,
                Duration::from_millis(50),
            );
            assert_eq!(image.size.width, (480.0 * scale).round() as u32);
            assert_eq!(image.size.height, (190.0 * scale).round() as u32);
            let file = format!("{name}-{}.png", (scale * 100.0) as u32);
            let mut encoder = png::Encoder::new(
                BufWriter::new(fs::File::create(directory.join(&file)).unwrap()),
                image.size.width,
                image.size.height,
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&image.rgba)
                .unwrap();
            html.push_str(&format!("<figure><figcaption>{name} · {}%</figcaption><img src=\"{file}\" alt=\"{name} subtitle overlay at {} percent scaling\"></figure>", (scale * 100.0) as u32, (scale * 100.0) as u32));
        }
    }
    html.push_str("</html>");
    fs::write(directory.join("index.html"), html).unwrap();
}
