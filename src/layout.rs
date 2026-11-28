/// 物理ピクセル座標上の矩形
#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// サイドバー各列の折りたたみ状態
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SidebarState {
    pub col1_collapsed: bool, // 1..5 列
    pub col2_collapsed: bool, // 6..0 列
}

impl SidebarState {
    pub fn is_col_collapsed(&self, col: usize) -> bool {
        match col {
            0 => self.col1_collapsed,
            1 => self.col2_collapsed,
            _ => false,
        }
    }

    pub fn toggle_col(&mut self, col: usize) {
        match col {
            0 => self.col1_collapsed = !self.col1_collapsed,
            1 => self.col2_collapsed = !self.col2_collapsed,
            _ => {}
        }
    }

    pub fn toggle_all(&mut self) {
        if self.col1_collapsed && self.col2_collapsed {
            self.col1_collapsed = false;
            self.col2_collapsed = false;
        } else {
            self.col1_collapsed = true;
            self.col2_collapsed = true;
        }
    }
}

/// サイドバーのクリック対象
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarHit {
    ToggleCol(usize), // 0 = 1..5列, 1 = 6..0列
    Slot(usize),      // 0..10
}

/// メイン上部ステータスバー上のクリック対象
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusBarHit {
    Back,
    Forward,
    Reload,
    UrlBar,
    OpenExternal,
    ToggleBrowser,
    SplitBrowser,
    ToggleHelp,
    UpgradeDaemon,
    Version,
    Timer,
    TimerIcon,
}

/// メイン上部ステータスバー内のボタン・URLバー矩形群
#[derive(Clone, Copy, Debug, Default)]
pub struct StatusBarControls {
    pub back_rect: Rect,
    pub forward_rect: Rect,
    pub reload_rect: Rect,
    pub url_bar_rect: Rect,
    pub clock_rect: Rect,
    pub version_rect: Rect,
    pub help_rect: Rect,
    pub ext_rect: Rect,
    pub toggle_mode_rect: Rect,
    pub split_web_rect: Rect,
    pub upgrade_daemon_rect: Rect,
    pub timer_rect: Rect,
    pub timer_icon_rect: Rect,
    pub mem_bar_rect: Rect,
    pub text_right_limit: f32,
}

/// ウィンドウ全体のレイアウト計算結果
#[derive(Clone, Debug)]
pub struct AppLayout {
    pub window_h: f32,
    pub main_rect: Rect,
    pub main_status_rect: Rect,
    pub main_footer_rect: Rect,
    pub main_inner_rect: Rect,
    pub sidebar_rect: Rect,
    pub sidebar_state: SidebarState,
    /// 各列上部の折りたたみトグルボタン矩形
    pub col_toggle_rects: [Rect; 2],
    /// 各スロットのカード（展開時）またはLEDセル（折りたたみ時）の矩形
    pub slot_rects: [Rect; 10],
    pub slot_header_rects: [Rect; 10],
    pub slot_preview_rects: [Rect; 10],
}

impl AppLayout {
    pub fn compute(width: u32, height: u32, scale: f32, sidebar_state: SidebarState) -> Self {
        let w = (width as f32).max(1.0);
        let h = (height as f32).max(1.0);

        let pad = 6.0 * scale;
        let gap = 6.0 * scale;
        let toggle_h = 20.0 * scale;
        let header_h = 22.0 * scale;

        // 展開時の1列幅（論理幅 145..210px）、折りたたみ時のLEDストリップ幅（論理幅 24px）
        let expanded_col_w = (w * 0.14).clamp(145.0 * scale, 210.0 * scale);
        let collapsed_col_w = 24.0 * scale;

        let col1_w = if sidebar_state.col1_collapsed {
            collapsed_col_w
        } else {
            expanded_col_w
        };
        let col2_w = if sidebar_state.col2_collapsed {
            collapsed_col_w
        } else {
            expanded_col_w
        };

        let sidebar_w = (pad * 2.0 + col1_w + gap + col2_w).min(w * 0.55);
        let main_w = (w - sidebar_w).max(1.0);

        let main_rect = Rect::new(0.0, 0.0, main_w, h);
        let status_h = pad + toggle_h;
        let main_status_rect = Rect::new(0.0, 0.0, main_w, status_h);
        let footer_h = (20.0 * scale).min((h - status_h).max(0.0));
        let main_footer_rect = Rect::new(0.0, (h - footer_h).max(status_h), main_w, footer_h);
        let main_pad = 8.0 * scale;
        let main_inner_rect = Rect::new(
            main_rect.x + main_pad,
            main_status_rect.bottom() + main_pad,
            (main_rect.w - main_pad * 2.0).max(1.0),
            (main_footer_rect.y - main_status_rect.bottom() - main_pad * 2.0).max(1.0),
        );

        let sidebar_rect = Rect::new(main_w, 0.0, sidebar_w, h);

        let col1_x = sidebar_rect.x + pad;
        let col2_x = col1_x + col1_w + gap;
        let col_rects = [
            Rect::new(col1_x, pad, col1_w, (h - pad * 2.0).max(1.0)),
            Rect::new(col2_x, pad, col2_w, (h - pad * 2.0).max(1.0)),
        ];
        let col_toggle_rects = [
            Rect::new(col1_x, pad, col1_w, toggle_h),
            Rect::new(col2_x, pad, col2_w, toggle_h),
        ];

        let slots_top = pad + toggle_h + gap;
        let slots_total_h = (h - slots_top - pad - gap * 4.0).max(5.0);
        let card_h = slots_total_h / 5.0;

        let mut slot_rects = [Rect::default(); 10];
        let mut slot_header_rects = [Rect::default(); 10];
        let mut slot_preview_rects = [Rect::default(); 10];

        for i in 0..10 {
            let col = i / 5; // 0: 1..5, 1: 6..0
            let row = i % 5; // 0..4 (上から下)
            let col_rect = col_rects[col];
            let x = col_rect.x;
            let y = slots_top + (row as f32) * (card_h + gap);
            let card_w = col_rect.w;

            let card = Rect::new(x, y, card_w, card_h);
            let header = Rect::new(x, y, card_w, header_h.min(card_h));
            let preview_pad = 3.0 * scale;
            let preview = Rect::new(
                x + preview_pad,
                y + header.h + preview_pad * 0.5,
                (card_w - preview_pad * 2.0).max(1.0),
                (card_h - header.h - preview_pad * 1.5).max(1.0),
            );

            slot_rects[i] = card;
            slot_header_rects[i] = header;
            slot_preview_rects[i] = preview;
        }

        Self {
            window_h: h,
            main_rect,
            main_status_rect,
            main_footer_rect,
            main_inner_rect,
            sidebar_rect,
            sidebar_state,
            col_toggle_rects,
            slot_rects,
            slot_header_rects,
            slot_preview_rects,
        }
    }

    /// サイドバー上のクリック対象を判定する
    pub fn hit_test_sidebar(&self, px: f32, py: f32) -> Option<SidebarHit> {
        for (col, rect) in self.col_toggle_rects.iter().enumerate() {
            if rect.contains(px, py) {
                return Some(SidebarHit::ToggleCol(col));
            }
        }
        for (i, rect) in self.slot_rects.iter().enumerate() {
            if rect.contains(px, py) {
                return Some(SidebarHit::Slot(i));
            }
        }
        None
    }

    /// メイン上部ステータスバーのボタン・URLバー矩形を計算する
    pub fn status_bar_controls(
        &self,
        scale: f32,
        is_browser: bool,
        show_daemon_upgrade: bool,
    ) -> StatusBarControls {
        let bar = self.main_status_rect;
        let pad_x = 8.0 * scale;
        let gap = 4.0 * scale;
        let btn_h = (bar.h - 6.0 * scale).max(14.0);
        let btn_y = bar.y + (bar.h - btn_h) * 0.5;

        let split_w = 46.0 * scale;
        let toggle_w = 46.0 * scale;
        let ext_w = 36.0 * scale;
        let help_w = 22.0 * scale;
        let upgrade_w = 68.0 * scale;
        let version_chars = env!("TENT_DISPLAY_VERSION").len().max(7) as f32;
        let version_w = (version_chars * 6.6 + 6.0) * scale;
        let clock_w = 208.0 * scale;

        // Wi-Fi＋バッテリー＋日時を右端へ右寄せ配置し、その左側にバージョンと操作ボタン群を並べる
        let clock_x = (bar.right() - pad_x - clock_w).max(bar.x);
        let clock_rect = Rect::new(clock_x, btn_y, clock_w, btn_h);

        let version_x = (clock_x - gap * 2.0 - version_w).max(bar.x);
        let version_rect = Rect::new(version_x, btn_y, version_w, btn_h);

        let help_x = (version_x - gap * 2.0 - help_w).max(bar.x);
        let split_x = (help_x - gap - split_w).max(bar.x);
        let toggle_x = (split_x - gap - toggle_w).max(bar.x);
        let ext_x = (toggle_x - gap - ext_w).max(bar.x);

        let help_rect = Rect::new(help_x, btn_y, help_w, btn_h);
        let split_web_rect = Rect::new(split_x, btn_y, split_w, btn_h);
        let toggle_mode_rect = Rect::new(toggle_x, btn_y, toggle_w, btn_h);
        let ext_rect = if is_browser {
            Rect::new(ext_x, btn_y, ext_w, btn_h)
        } else {
            Rect::default()
        };

        let left_anchor = if is_browser { ext_x } else { toggle_x };
        let (upgrade_daemon_rect, after_upgrade_x) = if show_daemon_upgrade {
            let ux = (left_anchor - gap - upgrade_w).max(bar.x);
            (Rect::new(ux, btn_y, upgrade_w, btn_h), ux)
        } else {
            (Rect::default(), left_anchor)
        };

        // ステータスバーの横長タイマー（右端=0分, 左端=60分）＋左隣の砂時計アイコン
        let timer_w = 132.0 * scale;
        let icon_w = 14.0 * scale;
        let timer_x = (after_upgrade_x - gap * 2.0 - timer_w).max(bar.x);
        let icon_x = (timer_x - gap - icon_w).max(bar.x);
        let (timer_rect, timer_icon_rect) =
            if after_upgrade_x - bar.x >= timer_w + icon_w + 185.0 * scale {
                (
                    Rect::new(timer_x, btn_y, timer_w, btn_h),
                    Rect::new(icon_x, btn_y, icon_w, btn_h),
                )
            } else {
                (Rect::default(), Rect::default())
            };
        let after_timer_x = if timer_rect.w > 0.0 {
            icon_x
        } else {
            after_upgrade_x
        };
        // メモリ使用量モニターバー（80% = 512MiB、100% = 640MiB）をタイマーの左隣に配置
        let mem_w = 76.0 * scale;
        let mem_x = (after_timer_x - gap * 2.0 - mem_w).max(bar.x);
        let mem_bar_rect = if after_timer_x - bar.x >= mem_w + 150.0 * scale {
            Rect::new(mem_x, btn_y, mem_w, btn_h)
        } else {
            Rect::default()
        };
        let text_right_limit = if mem_bar_rect.w > 0.0 {
            (mem_x - gap * 2.0).max(bar.x)
        } else if timer_rect.w > 0.0 {
            (icon_x - gap * 2.0).max(bar.x)
        } else {
            (after_upgrade_x - gap).max(bar.x)
        };

        if !is_browser {
            return StatusBarControls {
                clock_rect,
                version_rect,
                help_rect,
                split_web_rect,
                toggle_mode_rect,
                upgrade_daemon_rect,
                timer_rect,
                timer_icon_rect,
                mem_bar_rect,
                text_right_limit,
                ..StatusBarControls::default()
            };
        }

        let nav_w = 22.0 * scale;
        let mut x = bar.x + pad_x + 30.0 * scale; // "[1]" バッジ分
        let back_rect = Rect::new(x, btn_y, nav_w, btn_h);
        x += nav_w + gap;
        let forward_rect = Rect::new(x, btn_y, nav_w, btn_h);
        x += nav_w + gap;
        let reload_rect = Rect::new(x, btn_y, nav_w, btn_h);
        x += nav_w + gap * 1.5;

        let url_w = (text_right_limit - x).max(20.0);
        let url_bar_rect = Rect::new(x, btn_y, url_w, btn_h);

        StatusBarControls {
            back_rect,
            forward_rect,
            reload_rect,
            url_bar_rect,
            clock_rect,
            version_rect,
            help_rect,
            ext_rect,
            toggle_mode_rect,
            split_web_rect,
            upgrade_daemon_rect,
            timer_rect,
            timer_icon_rect,
            mem_bar_rect,
            text_right_limit,
        }
    }

    /// メイン上部ステータスバーのクリック対象を判定する
    pub fn hit_test_status_bar(
        &self,
        px: f32,
        py: f32,
        scale: f32,
        is_browser: bool,
        show_daemon_upgrade: bool,
    ) -> Option<StatusBarHit> {
        if !self.main_status_rect.contains(px, py) {
            return None;
        }
        let c = self.status_bar_controls(scale, is_browser, show_daemon_upgrade);
        if c.timer_icon_rect.w > 0.0 && c.timer_icon_rect.contains(px, py) {
            return Some(StatusBarHit::TimerIcon);
        }
        if c.timer_rect.w > 0.0 && c.timer_rect.contains(px, py) {
            return Some(StatusBarHit::Timer);
        }
        if show_daemon_upgrade && c.upgrade_daemon_rect.contains(px, py) {
            return Some(StatusBarHit::UpgradeDaemon);
        }
        if c.version_rect.contains(px, py) {
            return Some(StatusBarHit::Version);
        }
        if c.help_rect.contains(px, py) {
            return Some(StatusBarHit::ToggleHelp);
        }
        if c.split_web_rect.contains(px, py) {
            return Some(StatusBarHit::SplitBrowser);
        }
        if c.toggle_mode_rect.contains(px, py) {
            return Some(StatusBarHit::ToggleBrowser);
        }
        if is_browser {
            if c.ext_rect.contains(px, py) {
                return Some(StatusBarHit::OpenExternal);
            }
            if c.back_rect.contains(px, py) {
                return Some(StatusBarHit::Back);
            }
            if c.forward_rect.contains(px, py) {
                return Some(StatusBarHit::Forward);
            }
            if c.reload_rect.contains(px, py) {
                return Some(StatusBarHit::Reload);
            }
            if c.url_bar_rect.contains(px, py) {
                return Some(StatusBarHit::UrlBar);
            }
        }
        None
    }
}
