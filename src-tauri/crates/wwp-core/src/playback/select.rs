//! 选择器：从候选里随机挑下一张。
//!
//! 纯逻辑，不碰文件系统与系统接口，因此可以完整单测。
//!
//! ## 只有随机一种播放方式
//!
//! 原先还有"顺序 / 倒序"两种（按路径升序 / 降序走）。用户明确取消了它们，
//! 因此这里不再有"顺序"这个概念，界面上的顺序选项也一并删除。
//!
//! ## 随机为什么仍然要记一点东西
//!
//! 真正均匀随机的抽签会连续抽到同一张——一屏几十张候选里，刚看过的那张立刻
//! 再来一次的概率并不低，而用户会把它当成 bug。因此这里维护一个**最近看过的
//! 名单**：优先挑不在名单里的；候选比名单还少时才允许重复。
//!
//! 名单**只有 10 项**，而且**只记名字**（不记图像）。这是用户明确要求的长度，
//! 理由是：
//!
//! - 他不需要"一个周期内不重复"这种保证，也不需要记住整套播放顺序；
//! - 只要挡住"刚看过的那几张别马上又来"就够了——10 张在这个尺度上够用；
//! - 名字是几十字节的字符串，代价可以忽略（图像才是贵的那部分）。
//!
//! 名单同时充当"上一张"的来源：回退游标在名单里往回走。
//!
//! ## 重新加载 = 全新的一轮随机
//!
//! 名单只活在内存里，随播放计划一起重建（见 `Playback::new`）。
//! 因此每次「重新加载」都会得到一轮全新的随机序列——这正是用户要的行为。
//! 也不需要把顺序（或名单）持久化到磁盘。
//!
//! ## 「下一张」是可以预先算出来的（用户 2026-10 的要求）
//!
//! 播放页要显示"即将切换到的壁纸"。这就要求**预览与真正抽取是同一张**——
//! 显示一个跟实际不一样的"下一张"比不显示更糟。
//!
//! 做法是把抽签计数（[`Selector::tick`]）从全局静态变量搬进选择器自己：
//! 挑哪一张完全由 `(tick, 候选, 最近名单)` 决定，而这三样在两次抽取之间不会变。
//! 于是 [`Selector::peek_next`] 用同一套算式算出同一张，且**不消耗**这次抽签：
//! 预览 100 次之后 `next()` 拿到的仍然是预览的那一张。

use std::collections::VecDeque;

use crate::library::LibraryEntry;

/// 最近看过的名单长度。
///
/// 用户明确要求"只记前十个"。不要随手改大：它一旦接近候选数量，
/// 随机就会退化成"必须把整个图库走一遍才允许重复"，那正是被取消的行为。
pub const RECENT_LIMIT: usize = 10;

/// 每次抽取让 [`Selector::tick`] 前进的量。
///
/// 用一个奇数大常数（与模块里其它地方一致的黄金比例截断值）而不是 +1：
/// 相邻的计数器值经过哈希之后仍然要看起来随机。
const TICK_STEP: u64 = 0x9E37_79B9_7F4A_7C15;

/// 新选择器的起始计数。
///
/// **每个选择器从不同的起点出发**（而不是都从同一个常数开始）：
/// 两块屏用同一个播放列表时候选完全相同，若起点也一样，两块屏就会换出
/// 一模一样的顺序——而用户看到的是"两块屏同步了"。这里取一次全局序号，
/// 让每个新建的选择器落在自己的一段序列上。
fn next_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(TICK_STEP);
    SEED.fetch_add(TICK_STEP, Ordering::Relaxed)
}

/// 一块屏的选择器。
#[derive(Debug, Clone)]
pub struct Selector {
    /// 候选条目。顺序不重要（随机挑），因此不再排序。
    entries: Vec<LibraryEntry>,
    /// 最近播放过的**名字**，越靠前越新。只存字符串，不存图像。
    recent: VecDeque<String>,
    /// 回退游标：0 表示"当前显示的就是 `recent[0]`"，每按一次"上一张"加一。
    ///
    /// 用游标而不是"从名单里弹出去"：弹出去会让刚退掉的那张从"最近看过"里消失，
    /// 于是紧接着按"下一张"可能又抽中它，表现为"按了上一张再按下一张，画面没变"。
    back: usize,
    /// 抽签计数：每抽一次前进 [`TICK_STEP`]。
    ///
    /// 放在结构体里（而不是像原先那样用一个全局静态计数器）是为了让
    /// "下一张会是谁"可以被**预先算出**：预览与抽取必须用同一个计数，
    /// 否则界面上显示的"下一张"和真正换上的不是同一张。
    tick: u64,
    /// **已经预告出去**的下一张：删掉一张图之后要被钉住，不许换。
    ///
    /// ## 为什么需要它（用户 2026-10 报的 bug）
    ///
    /// 抽签的随机起点是 `hash(tick, 候选总数, 最近名单长度)`。而「删除当前壁纸」
    /// 会同时动到后两个输入：候选少一张、最近名单里把它去掉（名单于是短一项）。
    /// 于是**同一个 tick 算出了另一个起点**，页面上那张"下一张"当场变成别的图——
    /// 用户的感受就是"点删除之后，下一张重新生成了另一张"。
    ///
    /// 修法：`forget` 动手之前先把**当时预告的那张**记在这里；`pick_index` 优先返回它，
    /// `next()` 取走之后清空（那时它已经变成"当前"，新的预告重新按 hash 算）。
    /// 删掉的正好是预告的那张时钉不住（那张已经没了），此时如实换一张预告。
    pin: Option<String>,
}

impl Selector {
    /// 建立选择器。此时不挑任何图（第一次由调用方决定）。
    pub fn new(candidates: Vec<LibraryEntry>) -> Selector {
        Selector {
            entries: candidates,
            recent: VecDeque::new(),
            back: 0,
            tick: next_seed(),
            pin: None,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 这次会挑中哪个下标。**不改动任何状态**——`next` 与 `peek_next` 共用它，
    /// 这是"预览与实际一致"的唯一保证。
    fn pick_index(&self) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }

        // 钉选的优先：删过图之后，已经预告出去的那张必须继续是"下一张"。
        if let Some(pinned) = &self.pin {
            if let Some(index) = self.entries.iter().position(|entry| entry.p == *pinned) {
                return Some(index);
            }
        }

        let total = self.entries.len();
        let start = pseudo_random_index(self.tick, total, self.recent.len());

        // 从随机起点开始向后扫描，找第一张**不在**名单里的。
        // 名单里全是候选（候选比名单还少）时扫一圈都找不到，
        // 那就接受重复，用随机起点——绝不能在这里返回 None，
        // 那会让候选很少的屏"点了换纸却没反应"。
        let mut chosen = start;
        for offset in 0..total {
            let candidate = (start + offset) % total;
            if !self.recent.iter().any(|seen| seen == &self.entries[candidate].p) {
                chosen = candidate;
                break;
            }
        }
        Some(chosen)
    }

    /// **下一张**会是哪一张（供界面显示"即将切换到的壁纸"）。
    ///
    /// 它不消耗抽签、不改动任何状态：连着调 100 次得到同一张，
    /// 随后的 [`Selector::next`] 拿到的也正是这一张。
    pub fn peek_next(&self) -> Option<&LibraryEntry> {
        self.pick_index().map(|index| &self.entries[index])
    }

    /// 取下一张。候选为空时返回 `None`。
    pub fn next(&mut self) -> Option<&LibraryEntry> {
        let chosen = self.pick_index()?;
        self.tick = self.tick.wrapping_add(TICK_STEP);
        // 钉选只对**这一次**抽取有效：取走之后它就成了"当前"，
        // 新的预告该重新按 tick 算（否则会一直钉在同一张上）。
        self.pin = None;

        // 只记名字，且只留最近 10 个。
        //
        // 同一个名字在名单里只留一份（先挪到最前）：名单的语义是"最近看过的名字"，
        // 重复出现会让它虚长——候选很少时（只有 1~2 张）重复会把名单撑满，
        // 于是"上一张"会退到同一个名字上，表现为"按了没反应"。
        let path = self.entries[chosen].p.clone();
        if let Some(position) = self.recent.iter().position(|seen| seen == &path) {
            self.recent.remove(position);
        }
        self.recent.push_front(path);
        while self.recent.len() > RECENT_LIMIT {
            self.recent.pop_back();
        }
        // 新挑了一张，回退游标回到"当前这张"。
        self.back = 0;

        Some(&self.entries[chosen])
    }

    /// 取上一张。名单里没有更早的就返回 `None`。
    ///
    /// 名单里 `[0]` 是**当前正在显示**的那张，因此"上一张"是 `[1]`。
    /// 名单里只有一项时无法回退：返回 `None` 让调用方明确报告"没有可回退的上一张"，
    /// 而不是把当前这张再"回退"成它自己（那样点了没反应，比明确报告更糟）。
    pub fn previous(&mut self) -> Option<&LibraryEntry> {
        if self.back + 1 >= self.recent.len() {
            return None;
        }
        self.back += 1;
        let path = self.recent.get(self.back)?.clone();
        self.entries.iter().find(|entry| entry.p == path)
    }

    /// 这条路径**现在**还在候选里吗。
    ///
    /// 用在"重启后恢复上次的壁纸"上：记下来的路径可能已经被用户从图库里挪走，
    /// 那样就不能恢复它，得按现在的候选重新抽一张。
    pub fn contains(&self, path: &str) -> bool {
        self.entries.iter().any(|entry| entry.p == path)
    }

    /// 按路径取候选条目（不在候选里就是 `None`）。
    pub fn get(&self, path: &str) -> Option<&LibraryEntry> {
        self.entries.iter().find(|entry| entry.p == path)
    }

    /// 钉住"下一张"：抽取时优先取它，直到它被抽走或被移出候选。
    ///
    /// 重启后恢复记忆时用得上——上次预告给用户的那张，这次必须真的换上
    /// （否则界面上写着"下一张：A"，实际换了 B）。
    pub fn pin(&mut self, path: &str) {
        self.pin = Some(path.to_string());
    }

    /// 最近看过的名字（越靠前越新），供测试与排查用。
    pub fn recent(&self) -> Vec<String> {
        self.recent.iter().cloned().collect()
    }

    /// 把一张图**永久移出**候选（返回是否真的移掉了）。
    ///
    /// ## 什么时候用
    ///
    /// 播放页上的「删除当前壁纸」：用户把那张图移进了软件回收站，于是它
    /// **不能再被抽到**——否则下一次换纸就会去打开一个已经不存在的文件，
    /// 屏幕上还停在旧的图上，看起来像"删了但没用"。
    ///
    /// ## 必须保住"已经预告出去的那张"（**这里踩过一个真 bug**）
    ///
    /// 用户 2026-10 报：「点删除当前壁纸时，播放的下一张不是页面显示的下一张，
    /// 而是重新生成了一张。」根因在抽签的随机起点：
    /// `hash(tick, 候选总数, 最近名单长度)` —— 而删除**同时**动到了后两个输入
    /// （候选少一张、名单里把它去掉），于是同一个 tick 算出另一个起点，
    /// 页面上那张"下一张"当场变成别的图。
    ///
    /// 因此这里在动手之前先把当时预告的那张记进 [`Selector::pin`]：
    /// 预告与随后的抽取都优先取它。删掉的正好是预告的那张时钉不住（那张已经没了），
    /// 那时如实换一张预告——这是唯一允许预告改变的情况。
    ///
    /// 名单里还是要一起去掉：否则"上一张"会退到一张已经不存在的图上。
    /// 回退游标跟着夹到有效范围内（删掉当前这张之后，名单里的下一项
    /// 自然成为"当前"）。
    pub fn forget(&mut self, path: &str) -> bool {
        if !self.entries.iter().any(|entry| entry.p.eq_ignore_ascii_case(path)) {
            return false;
        }
        // 先算"现在预告的是哪张"，再动状态（顺序反了就钉到错的那张上）。
        let predicted = self.peek_next().map(|entry| entry.p.clone());

        let before = self.entries.len();
        self.entries
            .retain(|entry| !entry.p.eq_ignore_ascii_case(path));
        if before == self.entries.len() {
            // 只可能是同一张被删了两次。
            return false;
        }
        self.recent.retain(|seen| !seen.eq_ignore_ascii_case(path));
        if self.back >= self.recent.len() {
            self.back = self.recent.len().saturating_sub(1);
        }

        // 删掉的就是预告的那张 → 钉不住，如实让预告重新算一张。
        self.pin = predicted.filter(|pinned| !pinned.eq_ignore_ascii_case(path));
        true
    }
}

/// 一个够用的伪随机下标。
///
/// 不引入 `rand`：这里只需要"看起来随机、且不依赖时间/系统状态"，
/// 目的是让同一组输入产生可复现的序列（测试才能断言，预览才能与抽取一致）。
/// 用与其它模块一致的 FNV-1a 混合；`tick` 由调用方给（见 [`Selector::tick`]），
/// 不再读全局静态计数器——那种写法每次调用都会前进，因而**没法预览**。
fn pseudo_random_index(tick: u64, total: usize, recent_len: usize) -> usize {
    let mut value = tick ^ (recent_len as u64).wrapping_mul(0x100_0000_01B3);
    value ^= value >> 33;
    value = value.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    value ^= value >> 33;
    (value % total as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(count: usize) -> Vec<LibraryEntry> {
        (0..count)
            .map(|i| LibraryEntry {
                p: format!("C:/pics/{i:03}.png"),
                w: 100,
                h: 100,
                mt: 1,
                sz: 1,
                or: 1,
                fmt: "png".to_string(),
                ok: 1,
                kind: crate::library::index::MediaKind::Image,
            })
            .collect()
    }

    #[test]
    fn empty_candidates_yield_none() {
        let mut selector = Selector::new(Vec::new());
        assert!(selector.next().is_none());
        assert!(selector.previous().is_none());
        assert!(selector.is_empty());
    }

    #[test]
    fn the_recent_list_holds_only_names_and_only_ten_of_them() {
        // 用户明确要求："只记前十个、只记名称"。名单变长会让随机退化成
        // "整库走一遍才允许重复"，那正是被取消的行为。
        let mut selector = Selector::new(entries(200));
        for _ in 0..300 {
            selector.next();
        }
        let recent = selector.recent();
        assert_eq!(recent.len(), RECENT_LIMIT);
        assert!(
            recent.iter().all(|name| name.starts_with("C:/pics/")),
            "名单里存的应当是名字：{recent:?}"
        );
    }

    #[test]
    fn a_fresh_selector_starts_with_an_empty_recent_list() {
        // 「重新加载」会重建选择器，因此新一轮随机从空的名单开始——
        // 这就是"每次重新加载都生成新的随机顺序"的实现方式（不需要记住旧顺序）。
        let mut used = Selector::new(entries(50));
        for _ in 0..5 {
            used.next();
        }
        assert!(!used.recent().is_empty());

        let fresh = Selector::new(entries(50));
        assert!(fresh.recent().is_empty(), "重建后的名单必须是空的");
    }

    #[test]
    fn random_avoids_the_ten_most_recent_names() {
        // 候选 40 张、名单 10 项：最初 10 次抽到的应当互不相同。
        let mut selector = Selector::new(entries(40));
        let mut seen = std::collections::HashSet::new();
        for i in 0..RECENT_LIMIT {
            let name = selector.next().unwrap().p.clone();
            assert!(
                seen.insert(name.clone()),
                "第 {} 次抽到了重复的 {name}；名单：{:?}",
                i + 1,
                selector.recent()
            );
        }
        assert_eq!(seen.len(), RECENT_LIMIT);
    }

    #[test]
    fn random_repeats_when_candidates_are_fewer_than_the_recent_list() {
        // 候选只有 2 张时，第 3 次必然重复；不能因此死循环或返回 None。
        let mut selector = Selector::new(entries(2));
        for _ in 0..20 {
            assert!(selector.next().is_some());
        }
    }

    #[test]
    fn memory_is_bounded_for_a_single_candidate_too() {
        // 候选只有一张时名单里也只留一份，而且"上一张"要明确报告没有可回的——
        // 若名单里攒了 10 份同一个名字，"上一张"会退回同一张图，
        // 表现出来就是"点了没反应"。
        let mut selector = Selector::new(entries(1));
        for _ in 0..20 {
            assert_eq!(selector.next().unwrap().p, "C:/pics/000.png");
        }
        assert_eq!(selector.recent().len(), 1, "只有一张候选时名单也只有一项");
        assert!(selector.previous().is_none(), "退无可退时要返回 None");
    }

    #[test]
    fn the_recent_list_holds_each_name_once() {
        // 候选比名单少时"必然重复"，但名单不该被同一张图撑满。
        let mut selector = Selector::new(entries(3));
        for _ in 0..30 {
            selector.next();
        }
        let recent = selector.recent();
        assert_eq!(recent.len(), 3, "三张候选的名单最多三项：{recent:?}");
        let mut sorted = recent.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "名单里不该有重复名字");
    }

    #[test]
    fn previous_returns_to_the_previously_shown_image() {
        let mut selector = Selector::new(entries(40));
        let first = selector.next().unwrap().p.clone();
        let second = selector.next().unwrap().p.clone();
        let third = selector.next().unwrap().p.clone();
        assert_ne!(first, second);
        assert_ne!(second, third);

        // 当前显示的是 third；回退一次应回到 second，再回退回到 first。
        assert_eq!(selector.previous().unwrap().p, second);
        assert_eq!(selector.previous().unwrap().p, first);
    }

    #[test]
    fn previous_reports_nothing_to_go_back_to() {
        let mut selector = Selector::new(entries(5));
        selector.next();
        assert!(selector.previous().is_none(), "只有当前这一张时无法回退");
    }

    #[test]
    fn next_after_previous_does_not_get_stuck_on_the_same_image() {
        // 回归防护：回退之后紧接着"下一张"，不该又是刚退掉的那张。
        // 名单里仍然留着它（它是"最近看过"的），因此这一条自然成立。
        let mut selector = Selector::new(entries(40));
        for _ in 0..5 {
            selector.next();
        }
        let went_back = selector.previous().unwrap().p.clone();
        let after_next = selector.next().unwrap().p.clone();
        assert_ne!(
            after_next, went_back,
            "回退到 {went_back} 之后，下一张不应又是它"
        );
    }

    #[test]
    fn random_covers_multiple_distinct_positions_over_many_draws() {
        // 防止伪随机退化成"永远返回同一张"。
        let mut selector = Selector::new(entries(40));
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            seen.insert(selector.next().unwrap().p.clone());
        }
        assert!(
            seen.len() >= 20,
            "200 次抽取只覆盖了 {} 个位置，随机性不足",
            seen.len()
        );
    }

    #[test]
    fn a_forgotten_entry_is_never_drawn_again() {
        // **这条守的是「删除当前壁纸」。** 那张图已经被移走了，若它还留在候选里，
        // 下一次抽到它就会去打开一个不存在的文件——屏幕上还停在旧图上，
        // 用户看到的就是"删了但没用"。
        let mut selector = Selector::new(entries(6));
        let victim = selector.next().unwrap().p.clone();
        assert!(selector.forget(&victim), "候选里有它，应当移得掉");
        assert_eq!(selector.len(), 5);

        for _ in 0..200 {
            let next = selector.next().unwrap().p.clone();
            assert_ne!(next, victim, "被删掉的那张不该再被抽到");
        }
    }

    #[test]
    fn forgetting_an_unknown_path_changes_nothing() {
        let mut selector = Selector::new(entries(3));
        assert!(!selector.forget("C:/pics/does-not-exist.png"));
        assert_eq!(selector.len(), 3);
    }

    #[test]
    fn deleting_an_image_does_not_change_the_previewed_next_one() {
        // **这条守的是用户 2026-10 报的那个 bug。**
        // 他点「删除当前壁纸」之后，换上来的不是页面上显示的那张「下一张」，
        // 而是重新生成了另一张。根因：`forget` 原来直接从 `entries` 里移掉一项，
        // 而抽签是"由 tick 决定**下标**"——删掉一项之后，同一个下标
        // 指向的已经是另一张图，预告于是当场失效。
        let mut selector = Selector::new(entries(40));
        // 先走几张，让"当前"与"下一张"都确定下来。
        selector.next();
        let current = selector.recent()[0].clone();
        let previewed = selector.peek_next().unwrap().p.clone();

        // 删除"当前"这一张（界面上的「删除当前壁纸」走的就是这一步）。
        assert!(selector.forget(&current), "当前那张应当能在候选里找到");
        assert_ne!(previewed, current, "前提：预告的不是当前这张");

        assert_eq!(
            selector.peek_next().map(|entry| entry.p.clone()),
            Some(previewed.clone()),
            "删掉任意一张之后，预告的「下一张」不该变"
        );
        assert_eq!(
            selector.next().map(|entry| entry.p.clone()),
            Some(previewed),
            "换上的必须还是刚才预告的那一张"
        );
    }

    #[test]
    fn deleting_the_previewed_next_one_is_the_only_way_the_preview_moves() {
        // 反过来：删掉的**正是**预告的那张时，预告当然要换一张（那张已经没了）。
        let mut selector = Selector::new(entries(40));
        let previewed = selector.peek_next().unwrap().p.clone();
        assert!(selector.forget(&previewed));
        let after = selector.peek_next().map(|entry| entry.p.clone());
        assert_ne!(after, Some(previewed.clone()), "被删掉的不该还出现在预告里");
        assert_eq!(
            after,
            selector.next().map(|entry| entry.p.clone()),
            "换一张预告之后，它与真正的抽取仍然必须一致"
        );
    }

    #[test]
    fn a_second_forget_of_the_same_image_reports_nothing_removed() {
        let mut selector = Selector::new(entries(6));
        let victim = selector.next().unwrap().p.clone();
        assert!(selector.forget(&victim), "第一次删得掉");
        assert_eq!(selector.len(), 5);
        assert!(!selector.forget(&victim), "同一张删两次：第二次没什么可删的");
        assert_eq!(selector.len(), 5);
    }

    #[test]
    fn the_preview_is_exactly_what_comes_next() {
        // **这条守的是播放页上"下一张"那格显示的内容。**
        // 用户看到的"即将切换到的壁纸"必须与真正换上的那张是同一张——
        // 显示一个不准的预览比不显示更糟。
        let mut selector = Selector::new(entries(40));
        for round in 0..30 {
            let preview = selector.peek_next().map(|entry| entry.p.clone());
            let actual = selector.next().map(|entry| entry.p.clone());
            assert_eq!(preview, actual, "第 {} 轮：预览与实际抽到的不是同一张", round + 1);
        }
    }

    #[test]
    fn peeking_never_consumes_the_draw() {
        let mut selector = Selector::new(entries(10));
        let first = selector.peek_next().unwrap().p.clone();
        for _ in 0..100 {
            assert_eq!(
                selector.peek_next().unwrap().p,
                first,
                "连着预览不该换出别的图"
            );
        }
        assert_eq!(
            selector.next().unwrap().p,
            first,
            "预览了 100 次之后，真正抽到的仍然是预览的那一张"
        );
        // 抽过一次之后，预览应当变成"新的下一张"，而不是又给出刚抽走的那张。
        let next = selector.peek_next().unwrap().p.clone();
        assert_ne!(next, first, "刚抽走的那张不该又出现在预览里（名单会挡住它）");
    }

    #[test]
    fn the_preview_follows_going_back_and_forgetting() {
        // 「上一张」与「删除当前壁纸」都会动选择器，预览必须跟着动，
        // 而且仍然与随后真正抽到的那张一致。
        let mut selector = Selector::new(entries(40));
        selector.next();
        selector.next();
        selector.previous();
        assert_eq!(
            selector.peek_next().map(|entry| entry.p.clone()),
            selector.next().map(|entry| entry.p.clone()),
            "回退之后的预览也要准"
        );

        let victim = selector.peek_next().unwrap().p.clone();
        assert!(selector.forget(&victim), "预览里的这张应当还在候选里");
        let after_forget = selector.peek_next().map(|entry| entry.p.clone());
        assert_ne!(after_forget, Some(victim), "被删掉的那张不该还出现在预览里");
        assert_eq!(
            after_forget,
            selector.next().map(|entry| entry.p.clone()),
            "删掉一张之后的预览同样要准"
        );
    }

    #[test]
    fn an_empty_selector_previews_nothing() {
        let selector = Selector::new(Vec::new());
        assert!(selector.peek_next().is_none());
    }

    #[test]
    fn two_screens_with_the_same_candidates_do_not_preview_the_same_sequence() {
        // 两块屏用同一个播放列表时候选完全一样。若每个选择器都从同一个计数
        // 起点出发，两块屏会换出一模一样的顺序（用户会看到"两块屏同步了"）。
        let a = Selector::new(entries(40));
        let b = Selector::new(entries(40));
        let mut a = a;
        let mut b = b;
        let a_seq: Vec<String> = (0..8).map(|_| a.next().unwrap().p.clone()).collect();
        let b_seq: Vec<String> = (0..8).map(|_| b.next().unwrap().p.clone()).collect();
        assert_ne!(a_seq, b_seq, "两块屏的随机序列不该完全相同");
    }

    #[test]
    fn forgetting_the_current_image_keeps_previous_usable() {
        // 删掉的正好是"当前显示"的那张时，名单里也必须有它被清掉，
        // 否则"上一张"会退到一张已经不存在的图上（点了没反应）。
        let mut selector = Selector::new(entries(40));
        let first = selector.next().unwrap().p.clone();
        let second = selector.next().unwrap().p.clone();
        assert!(selector.forget(&second), "当前这张也能被移出候选");
        assert!(
            !selector.recent().iter().any(|name| name == &second),
            "名单里不该再留着它"
        );
        // 名单里只剩 first 了，它同时也是"当前"——因此退无可退要返回 None，
        // 而不是把 first 当成"上一张"再退一次（列表里没有更早的了）。
        assert!(selector.previous().is_none());
        assert_eq!(selector.recent(), vec![first], "剩下的那张还在名单里");
    }

    #[test]
    fn forgetting_everything_leaves_an_empty_selector() {
        let mut selector = Selector::new(entries(3));
        for i in 0..3 {
            assert!(selector.forget(&format!("C:/pics/{i:03}.png")));
        }
        assert!(selector.is_empty());
        assert!(selector.next().is_none(), "空了要返回 None 而不是崩掉");
    }
}
