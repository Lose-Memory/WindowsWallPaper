//! 全局快捷键：解析与注册。
//!
//! ## 为什么自己调 `RegisterHotKey` 而不是引一个库
//!
//! 项目的硬约束是"不在项目之外下载任何东西"，而 `RegisterHotKey` 是 Windows
//! 自带的能力。真正要注意的只有一件事：**注册必须在拥有消息队列的那个线程上做**，
//! 而且 `WM_HOTKEY` 只会投递到注册它的那个窗口。因此注册的入口放在
//! [`crate::hidden_window`] 里（那里已经有一个专用的消息窗口线程），
//! 这里只负责"字符串 → 修饰键 + 虚拟键"的解析，以及一对薄薄的封装。
//!
//! ## 快捷键的写法
//!
//! `Ctrl+Alt+S` 这样：`Ctrl` / `Alt` / `Shift` / `Win` 用 `+` 连接，
//! 最后一个是**单个字母、单个数字、F1~F24，或方向键一类有名字的键**
//! （`Left` / `Right` / `Space` / `PageUp` 等，完整清单见 [`parse_key`]）。大小写不敏感。
//!
//! 至少要有**一个修饰键**：`RegisterHotKey` 允许注册一个裸的字母键，
//! 那会把用户的 A 键从所有程序手里抢走——这种事故不能靠"用户不会这么写"来避免。

use std::fmt;

/// 至少一个修饰键，加上一个虚拟键码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    /// `MOD_ALT | MOD_CONTROL | MOD_SHIFT | MOD_WIN` 的组合。
    pub mods: u32,
    /// 虚拟键码（`VK_*`）。
    pub vk: u32,
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.mods & MOD_CONTROL != 0 {
            parts.push("Ctrl".to_string());
        }
        if self.mods & MOD_ALT != 0 {
            parts.push("Alt".to_string());
        }
        if self.mods & MOD_SHIFT != 0 {
            parts.push("Shift".to_string());
        }
        if self.mods & MOD_WIN != 0 {
            parts.push("Win".to_string());
        }
        parts.push(vk_name(self.vk));
        write!(f, "{}", parts.join("+"))
    }
}

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
/// 长按不重复触发。按住 NSFW 快捷键不放时不该反复切模式。
pub const MOD_NOREPEAT: u32 = 0x4000;

/// 五个全局快捷键的 id。
///
/// id 是注册与分发的唯一对应关系：`WM_HOTKEY` 只带回一个整数，靠它才知道
/// 用户按的是哪一个。因此两处（注册、分发）必须用同一份常量。
pub const NSFW_HOTKEY_ID: i32 = 1;
pub const PAUSE_HOTKEY_ID: i32 = 2;
pub const NEXT_HOTKEY_ID: i32 = 3;
pub const PREVIOUS_HOTKEY_ID: i32 = 4;
/// 「显示主界面」。不改变壁纸，只把设置窗口翻出来（与托盘「打开主界面」、
/// 第二次启动程序走的是同一段逻辑）。
pub const SHOW_WINDOW_HOTKEY_ID: i32 = 5;
/// 「切换播放列表」：每块配好两个列表的屏在两者之间往返一次。
pub const SWITCH_PLAYLISTS_HOTKEY_ID: i32 = 6;

/// 六个全局快捷键的写法（字符串形式，未解析）。空串 = 不注册。
///
/// 打包成一个结构而不是五个参数：注册、重新注册、日志三处都要用到它们，
/// 逐个传参会很容易漏掉一个，而漏掉的表现是"那个快捷键没生效"——
/// 从代码上看不出来。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotkeySpecs {
    pub nsfw: String,
    pub pause: String,
    pub next: String,
    pub previous: String,
    /// 显示主界面。见 [`SHOW_WINDOW_HOTKEY_ID`]。
    pub show_window: String,
    /// 切换播放列表。见 [`SWITCH_PLAYLISTS_HOTKEY_ID`]。
    pub switch_playlists: String,
}

impl HotkeySpecs {
    /// 逐个快捷键：(id, 界面上叫什么, 写法)。
    ///
    /// 注册与日志共用这一份，避免"注册了但日志写的是另一个键"。
    pub fn entries(&self) -> [(i32, &'static str, &str); 6] {
        [
            (NSFW_HOTKEY_ID, "NSFW 模式", self.nsfw.as_str()),
            (PAUSE_HOTKEY_ID, "暂停/继续", self.pause.as_str()),
            (NEXT_HOTKEY_ID, "下一张", self.next.as_str()),
            (PREVIOUS_HOTKEY_ID, "上一张", self.previous.as_str()),
            (SHOW_WINDOW_HOTKEY_ID, "显示主界面", self.show_window.as_str()),
            (
                SWITCH_PLAYLISTS_HOTKEY_ID,
                "切换播放列表",
                self.switch_playlists.as_str(),
            ),
        ]
    }

    /// 全部 id，注销时用。
    pub fn ids() -> [i32; 6] {
        [
            NSFW_HOTKEY_ID,
            PAUSE_HOTKEY_ID,
            NEXT_HOTKEY_ID,
            PREVIOUS_HOTKEY_ID,
            SHOW_WINDOW_HOTKEY_ID,
            SWITCH_PLAYLISTS_HOTKEY_ID,
        ]
    }
}

impl From<&crate::config::General> for HotkeySpecs {
    fn from(general: &crate::config::General) -> HotkeySpecs {
        HotkeySpecs {
            nsfw: general.nsfw_hotkey.clone(),
            pause: general.pause_hotkey.clone(),
            next: general.next_hotkey.clone(),
            previous: general.previous_hotkey.clone(),
            show_window: general.show_window_hotkey.clone(),
            switch_playlists: general.switch_playlists_hotkey.clone(),
        }
    }
}

/// 解析一个快捷键字符串。
///
/// 失败时返回的是**给用户看的原因**，会被原样写进日志与界面提示：
/// 用户打错字时最需要的是"哪一段看不懂"，而不是一个 `None`。
pub fn parse(spec: &str) -> Result<Hotkey, String> {
    let text = spec.trim();
    if text.is_empty() {
        return Err("快捷键为空".to_string());
    }

    let mut mods = 0u32;
    let mut key: Option<u32> = None;

    for (index, raw) in text.split('+').enumerate() {
        let token = raw.trim();
        if token.is_empty() {
            return Err(format!("「{}」里有多余的 +", text));
        }
        let lower = token.to_ascii_lowercase();
        let is_last = index == text.split('+').count() - 1;

        match lower.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" | "meta" => mods |= MOD_WIN,
            _ => {
                if !is_last {
                    return Err(format!("看不懂的修饰键「{token}」"));
                }
                if key.is_some() {
                    return Err("只能有一个主键".to_string());
                }
                key = Some(parse_key(token)?);
            }
        }
    }

    let vk = key.ok_or_else(|| format!("「{text}」里没有主键（最后一个应当是字母、数字或 F1~F24）"))?;
    if mods == 0 {
        return Err(format!(
            "「{text}」没有修饰键。全局快捷键至少要带 Ctrl / Alt / Shift / Win 之一，否则会抢走普通按键"
        ));
    }
    Ok(Hotkey { mods, vk })
}

/// 有名字的键：写法 → 虚拟键码。
///
/// ## 为什么需要它们
///
/// 只有字母/数字/F1~F24 时，"下一张/上一张"只能绑成 `Ctrl+Alt+N` 这种
/// 与语义无关的字母；而 `Left` / `Right` 才是这两个动作自然的键。
/// 这些键的虚拟键码都是固定值，列出来即可，不需要额外依赖。
///
/// 表里的名字同时是**回显用的规范写法**（`Hotkey::to_string` 用它）。
const NAMED_KEYS: &[(&str, u32)] = &[
    ("backspace", 0x08),
    ("tab", 0x09),
    ("enter", 0x0D),
    ("return", 0x0D),
    ("esc", 0x1B),
    ("escape", 0x1B),
    ("space", 0x20),
    ("pageup", 0x21),
    ("pagedown", 0x22),
    ("end", 0x23),
    ("home", 0x24),
    ("left", 0x25),
    ("up", 0x26),
    ("right", 0x27),
    ("down", 0x28),
    ("insert", 0x2D),
    ("delete", 0x2E),
];

/// 单个主键 → 虚拟键码。
fn parse_key(token: &str) -> Result<u32, String> {
    // 单个字母或数字。
    let mut chars = token.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        let upper = c.to_ascii_uppercase();
        if upper.is_ascii_uppercase() || upper.is_ascii_digit() {
            return Ok(upper as u32);
        }
    }
    let lower = token.to_ascii_lowercase();
    // F1 ~ F24。
    if let Some(rest) = lower.strip_prefix('f') {
        if let Ok(number) = rest.parse::<u32>() {
            if (1..=24).contains(&number) {
                // VK_F1 = 0x70
                return Ok(0x70 + number - 1);
            }
            return Err(format!("F{number} 超出范围（F1 ~ F24）"));
        }
    }
    // 有名字的键（方向键等）。
    if let Some((_, vk)) = NAMED_KEYS.iter().find(|(name, _)| *name == lower) {
        return Ok(*vk);
    }
    Err(format!(
        "看不懂的主键「{token}」（支持单个字母、单个数字、F1~F24，\
         以及 {} 这类有名字的键）",
        NAMED_KEYS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(" / ")
    ))
}

/// 虚拟键码 → 给人看的名字（用于回显规范化后的快捷键）。
fn vk_name(vk: u32) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk).map(|c| c.to_string()).unwrap_or_default(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        other => NAMED_KEYS
            .iter()
            .find(|(_, code)| *code == other)
            // 别名（return / escape）会命中同一个键码，因此取表里**第一个**
            // 匹配项就是规范写法（Enter / Esc）。
            .map(|(name, _)| canonical_name(name))
            .unwrap_or_else(|| format!("VK{other:02X}")),
    }
}

/// 别名 → 规范写法。
fn canonical_name(name: &str) -> String {
    match name {
        "return" => "Enter".to_string(),
        "escape" => "Esc".to_string(),
        "backspace" => "Backspace".to_string(),
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_style_spec_parses() {
        let hotkey = parse("Ctrl+Alt+S").unwrap();
        assert_eq!(hotkey.mods, MOD_CONTROL | MOD_ALT);
        assert_eq!(hotkey.vk, 0x53); // 'S'
        assert_eq!(hotkey.to_string(), "Ctrl+Alt+S");
    }

    #[test]
    fn parsing_is_case_and_space_insensitive() {
        let a = parse("ctrl + alt + s").unwrap();
        let b = parse("CTRL+ALT+S").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn digits_and_function_keys_are_supported() {
        assert_eq!(parse("Ctrl+Shift+7").unwrap().vk, 0x37);
        assert_eq!(parse("Ctrl+F12").unwrap().vk, 0x7B); // VK_F12
        assert_eq!(parse("Win+Alt+F24").unwrap().mods, MOD_WIN | MOD_ALT);
    }

    #[test]
    fn a_hotkey_without_modifiers_is_rejected_with_a_reason() {
        // 裸键会被 RegisterHotKey 真的抢走（用户的 A 键在别的程序里就不灵了），
        // 因此必须在解析阶段就拒绝，而且要说清为什么。
        let error = parse("S").unwrap_err();
        assert!(error.contains("修饰键"), "错误信息要说明原因：{error}");
    }

    #[test]
    fn unknown_tokens_are_reported_with_the_offending_text() {
        let error = parse("Ctrl+Bogus").unwrap_err();
        assert!(error.contains("Bogus"), "要指出是哪一段看不懂：{error}");
        assert!(parse("Ctrl+").is_err());
        assert!(parse("").is_err());
        assert!(parse("Ctrl+F99").is_err());
    }

    #[test]
    fn duplicate_modifiers_are_harmless() {
        // 用户手滑写了两次 Ctrl 不该报错——它的意图是明确的。
        let hotkey = parse("Ctrl+Ctrl+Alt+S").unwrap();
        assert_eq!(hotkey.mods, MOD_CONTROL | MOD_ALT);
    }

    #[test]
    fn named_keys_are_supported_and_echoed_canonically() {
        // "上一张/下一张"绑方向键才是自然的写法，因此解析必须支持它们。
        let next = parse("Ctrl+Alt+Right").unwrap();
        assert_eq!(next.vk, 0x27, "VK_RIGHT");
        assert_eq!(next.to_string(), "Ctrl+Alt+Right");

        let previous = parse("ctrl+alt+left").unwrap();
        assert_eq!(previous.vk, 0x25, "VK_LEFT");
        assert_eq!(previous.to_string(), "Ctrl+Alt+Left");

        // 别名与大小写都要能吃进来，并且回显成规范写法。
        assert_eq!(parse("Ctrl+Space").unwrap().vk, 0x20);
        assert_eq!(parse("Ctrl+Space").unwrap().to_string(), "Ctrl+Space");
        assert_eq!(parse("Win+Escape").unwrap().to_string(), "Win+Esc");
        assert_eq!(parse("Alt+Return").unwrap().to_string(), "Alt+Enter");
        assert_eq!(parse("Ctrl+PageUp").unwrap().to_string(), "Ctrl+PageUp");
    }

    #[test]
    fn specs_bundle_covers_exactly_the_six_ids() {
        // 注册与分发靠 id 对上号；两边不一致的表现是"某个快捷键按了没反应"，
        // 而那种问题从代码上很难看出来，因此用这条测试钉住这份对应关系。
        let specs = HotkeySpecs {
            nsfw: "Ctrl+Alt+S".to_string(),
            pause: "Ctrl+Alt+P".to_string(),
            next: "Ctrl+Alt+Right".to_string(),
            previous: "Ctrl+Alt+Left".to_string(),
            show_window: "Ctrl+Alt+W".to_string(),
            switch_playlists: "Ctrl+Alt+Shift+L".to_string(),
        };
        let entries = specs.entries();
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[0].0, NSFW_HOTKEY_ID);
        assert_eq!(entries[1].0, PAUSE_HOTKEY_ID);
        assert_eq!(entries[2].0, NEXT_HOTKEY_ID);
        assert_eq!(entries[3].0, PREVIOUS_HOTKEY_ID);
        assert_eq!(entries[4].0, SHOW_WINDOW_HOTKEY_ID);
        assert_eq!(entries[5].0, SWITCH_PLAYLISTS_HOTKEY_ID);
        assert_eq!(entries[3].2, "Ctrl+Alt+Left");
        assert_eq!(entries[4].2, "Ctrl+Alt+W");
        assert_eq!(entries[5].2, "Ctrl+Alt+Shift+L");

        let mut ids: Vec<i32> = HotkeySpecs::ids().to_vec();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 6, "六个 id 不能重复");
        // 每个写法都必须能解析，否则默认配置本身就是坏的。
        for (_, label, spec) in entries {
            assert!(parse(spec).is_ok(), "{label} 的默认写法应当能解析：{spec}");
        }
    }

    #[test]
    fn default_general_gives_six_parsable_hotkeys() {
        // 升级路径：旧设置文件里没有这几个新字段，会取默认值。
        // 默认值必须是能注册的写法——否则用户一升级就有一条 warn 日志，
        // 而"下一张/上一张"看起来像没做。
        let general = crate::config::General::default();
        let specs = HotkeySpecs::from(&general);
        assert_eq!(specs.entries().len(), 6);
        for (_, label, spec) in specs.entries() {
            let parsed = parse(spec).unwrap_or_else(|e| panic!("{label} 默认值无法解析：{e}"));
            assert_eq!(parsed.to_string(), spec, "{label} 的默认写法应当已是规范写法");
        }
    }
}
