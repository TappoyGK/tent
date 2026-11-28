use glyphon::{Attrs, Buffer, Color, Family, Metrics, Shaping, TextArea, Weight};

use crate::layout::Rect;

use super::primitives::{RectInstance, build_help_spans, safe_bounds};
use super::{RenderOverlays, Renderer};

impl Renderer {
    pub(super) fn build_help_modal(
        &mut self,
        scale_factor: f32,
        overlays: &RenderOverlays<'_>,
        rects: &mut Vec<RectInstance>,
    ) -> Option<Rect> {
        // ヘルプダイアログ表示時は全画面ディム＋中央モーダルカードを構築
        let mut help_modal_rect: Option<Rect> = None;
        if overlays.show_help {
            let win_w = self.config.width as f32;
            let win_h = self.config.height as f32;
            self.push_rect(
                rects,
                Rect::new(0.0, 0.0, win_w, win_h),
                [0.02, 0.03, 0.05, 0.80],
            );

            let modal_w = (760.0 * scale_factor)
                .min(win_w - 40.0 * scale_factor)
                .max(320.0);
            let modal_h = (460.0 * scale_factor)
                .min(win_h - 40.0 * scale_factor)
                .max(260.0);
            let mx = (win_w - modal_w) * 0.5;
            let my = (win_h - modal_h) * 0.5;
            let modal = Rect::new(mx, my, modal_w, modal_h);
            let header_h = 34.0 * scale_factor;

            self.push_rect(rects, modal, [0.09, 0.11, 0.15, 0.98]);
            self.push_rect(
                rects,
                Rect::new(modal.x, modal.y, modal.w, header_h),
                [0.12, 0.15, 0.21, 1.0],
            );
            self.push_rect(
                rects,
                Rect::new(modal.x, modal.y + header_h, modal.w, 1.0 * scale_factor),
                [0.24, 0.29, 0.38, 1.0],
            );
            // 中央の縦区切り線
            self.push_rect(
                rects,
                Rect::new(
                    modal.x + modal.w * 0.5,
                    modal.y + header_h + 12.0 * scale_factor,
                    1.0 * scale_factor,
                    modal.h - header_h - 24.0 * scale_factor,
                ),
                [0.19, 0.23, 0.30, 1.0],
            );
            self.push_border(rects, modal, 1.5 * scale_factor, [0.36, 0.68, 0.96, 0.90]);

            let hkey = (
                modal_w.round() as u32,
                (scale_factor * 100.0).round() as u32,
            );
            if self.help_cache_key != hkey {
                let title_fs = (13.0 * scale_factor).round().max(11.0);
                let title_metrics = Metrics::new(title_fs, (title_fs * 1.4).ceil());
                self.help_header_buffer.set_metrics(title_metrics);
                self.help_header_buffer
                    .set_size(Some(modal_w - 28.0 * scale_factor), Some(header_h));
                let base = Attrs::new()
                    .family(Family::Name("Menlo"))
                    .color(Color::rgb(0xe8, 0xee, 0xf8));
                self.help_header_buffer.set_rich_text(
                    [
                        (
                            "TenT Shortcuts",
                            Attrs::new()
                                .family(Family::Name("Menlo"))
                                .color(Color::rgb(0x5c, 0xad, 0xf5))
                                .weight(Weight::BOLD),
                        ),
                        (
                            "    Esc / Cmd+? / Click to close",
                            Attrs::new()
                                .family(Family::Name("Menlo"))
                                .color(Color::rgb(0x7c, 0x86, 0x9b)),
                        ),
                    ],
                    &base,
                    Shaping::Basic,
                    None,
                );
                self.help_header_buffer
                    .shape_until_scroll(&mut self.font_system, false);

                let body_fs = (11.5 * scale_factor).round().max(10.0);
                let body_metrics = Metrics::new(body_fs, (body_fs * 1.65).ceil());
                let col_w = (modal_w * 0.5 - 28.0 * scale_factor).max(120.0);
                let col_h = (modal_h - header_h - 24.0 * scale_factor).max(100.0);

                let left_text = concat!(
                    "[ Slots & Sidebar ]\n",
                    "  Cmd+1..9 / 0     スロット切替（再押しで隣列と往復）\n",
                    "  Cmd+Opt+1 / 2    1-5列 / 6-0列 折りたたみ\n",
                    "  Cmd+\\            サイドバー全列トグル\n",
                    "  Sidebar D&D      スロット入替\n",
                    "\n",
                    "[ Pane Splits ]\n",
                    "  Cmd+D            左右分割\n",
                    "  Cmd+Shift+D      上下分割\n",
                    "  Cmd+W            ペイン / ブラウザを閉じる\n",
                    "  Cmd+[ / Cmd+]    前後のペインへ移動\n",
                    "  Cmd+Opt+Arrow    方向指定でペイン移動\n",
                    "  Cmd+Ctrl+Arrow   分割ペイン境界をリサイズ\n",
                );
                let right_text = concat!(
                    "[ Browser ]\n",
                    "  Cmd+B            ターミナル ⇔ ブラウザ切替\n",
                    "  Cmd+Shift+B      ブラウザペインを左右分割で追加\n",
                    "  Cmd+L            URL バーへフォーカス（全選択）\n",
                    "  Cmd+R            ページ再読み込み\n",
                    "  Cmd+Left / Right 戻る / 進む（スワイプ対応）\n",
                    "\n",
                    "[ Terminal & App ]\n",
                    "  Cmd+Click        URL を内蔵ブラウザで開く\n",
                    "  Cmd+Shift+Click  URL を外部ブラウザで開く\n",
                    "  File D&D         スクショ等のパスを貼り付け\n",
                    "  Cmd+C / Cmd+V    コピー / ペースト\n",
                    "  Cmd+F            ペイン内検索（Enter / Shift+Enter / Cmd+G / Cmd+Shift+G / Esc）\n",
                    "  Cmd+K            画面・スクロールバックをクリア\n",
                    "  Cmd+Opt+R        TenT 本体再起動\n",
                    "  Cmd+/ / Cmd+?    このヘルプを表示 / 閉じる\n",
                );

                for (buf, txt) in [
                    (&mut self.help_left_buffer, left_text),
                    (&mut self.help_right_buffer, right_text),
                ] {
                    buf.set_metrics(body_metrics);
                    buf.set_size(Some(col_w), Some(col_h));
                    let spans = build_help_spans(txt);
                    let refs: Vec<(&str, Attrs<'_>)> =
                        spans.iter().map(|(s, a)| (s.as_str(), a.clone())).collect();
                    buf.set_rich_text(refs, &base, Shaping::Advanced, None);
                    buf.shape_until_scroll(&mut self.font_system, false);
                }

                self.help_cache_key = hkey;
            }
            help_modal_rect = Some(modal);
        }
        help_modal_rect
    }
}

pub(super) fn push_help_modal_text_areas<'a>(
    help_header_buffer: &'a Buffer,
    help_left_buffer: &'a Buffer,
    help_right_buffer: &'a Buffer,
    scale_factor: f32,
    help_modal_rect: Option<Rect>,
    text_areas: &mut Vec<TextArea<'a>>,
) {
    // ヘルプダイアログ表示時は下層テキストを隠してモーダル内テキストのみ前面描画
    if let Some(modal) = help_modal_rect {
        let header_h = 34.0 * scale_factor;
        text_areas.clear();
        text_areas.push(TextArea {
            buffer: help_header_buffer,
            left: modal.x + 16.0 * scale_factor,
            top: modal.y + 9.0 * scale_factor,
            scale: 1.0,
            bounds: safe_bounds(modal.x, modal.y, modal.right(), modal.y + header_h),
            default_color: Color::rgb(0xef, 0xf4, 0xff),
            custom_glyphs: &[],
        });
        let body_top = modal.y + header_h + 14.0 * scale_factor;
        let mid_x = modal.x + modal.w * 0.5;
        text_areas.push(TextArea {
            buffer: help_left_buffer,
            left: modal.x + 16.0 * scale_factor,
            top: body_top,
            scale: 1.0,
            bounds: safe_bounds(
                modal.x,
                body_top,
                mid_x - 8.0 * scale_factor,
                modal.bottom(),
            ),
            default_color: Color::rgb(0xd8, 0xde, 0xe9),
            custom_glyphs: &[],
        });
        text_areas.push(TextArea {
            buffer: help_right_buffer,
            left: mid_x + 16.0 * scale_factor,
            top: body_top,
            scale: 1.0,
            bounds: safe_bounds(
                mid_x,
                body_top,
                modal.right() - 8.0 * scale_factor,
                modal.bottom(),
            ),
            default_color: Color::rgb(0xd8, 0xde, 0xe9),
            custom_glyphs: &[],
        });
    }
}
