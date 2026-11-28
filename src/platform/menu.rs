#[cfg(target_os = "macos")]
use objc2::sel;
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
#[cfg(target_os = "macos")]
use objc2_foundation::{MainThreadMarker, NSString};

/// macOS 標準のメニューバー（App メニュー・Edit メニュー）を構築して `NSApp.mainMenu` へ設定する。
/// Edit メニューの target は `nil`（First Responder ルーティング）にするため、
/// `WKWebView` / `NSTextField` フォーカス中の `Cmd+C/V/X/A/Z/Shift+Z` が AppKit 正規経路でそのまま動作する。
#[cfg(target_os = "macos")]
pub fn install_main_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let main_menu = NSMenu::new(mtm);

    // 1. アプリケーションメニュー
    let app_menu_item = NSMenuItem::new(mtm);
    let app_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("TenT"));
    unsafe {
        let about_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("About TenT"),
            Some(sel!(orderFrontStandardAboutPanel:)),
            &NSString::from_str(""),
        );
        app_menu.addItem(&about_item);

        app_menu.addItem(&NSMenuItem::separatorItem(mtm));

        let services_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Services"),
            None,
            &NSString::from_str(""),
        );
        let services_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("Services"));
        services_item.setSubmenu(Some(&services_menu));
        app.setServicesMenu(Some(&services_menu));
        app_menu.addItem(&services_item);

        app_menu.addItem(&NSMenuItem::separatorItem(mtm));

        let hide_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Hide TenT"),
            Some(sel!(hide:)),
            &NSString::from_str("h"),
        );
        app_menu.addItem(&hide_item);

        let hide_others = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Hide Others"),
            Some(sel!(hideOtherApplications:)),
            &NSString::from_str("h"),
        );
        hide_others.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
        );
        app_menu.addItem(&hide_others);

        let show_all = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Show All"),
            Some(sel!(unhideAllApplications:)),
            &NSString::from_str(""),
        );
        app_menu.addItem(&show_all);

        app_menu.addItem(&NSMenuItem::separatorItem(mtm));

        let quit_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Quit TenT"),
            Some(sel!(terminate:)),
            &NSString::from_str("q"),
        );
        app_menu.addItem(&quit_item);
    }
    app_menu_item.setSubmenu(Some(&app_menu));
    main_menu.addItem(&app_menu_item);

    // 2. Edit メニュー（target = nil → First Responder へ自動ルーティング）
    let edit_menu_item = NSMenuItem::new(mtm);
    let edit_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("Edit"));
    unsafe {
        let undo = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Undo"),
            Some(sel!(undo:)),
            &NSString::from_str("z"),
        );
        edit_menu.addItem(&undo);

        let redo = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Redo"),
            Some(sel!(redo:)),
            &NSString::from_str("Z"),
        );
        edit_menu.addItem(&redo);

        edit_menu.addItem(&NSMenuItem::separatorItem(mtm));

        let cut = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Cut"),
            Some(sel!(cut:)),
            &NSString::from_str("x"),
        );
        edit_menu.addItem(&cut);

        let copy = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Copy"),
            Some(sel!(copy:)),
            &NSString::from_str("c"),
        );
        edit_menu.addItem(&copy);

        let paste = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Paste"),
            Some(sel!(paste:)),
            &NSString::from_str("v"),
        );
        edit_menu.addItem(&paste);

        let paste_plain = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Paste and Match Style"),
            Some(sel!(pasteAsPlainText:)),
            &NSString::from_str("V"),
        );
        edit_menu.addItem(&paste_plain);

        let select_all = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Select All"),
            Some(sel!(selectAll:)),
            &NSString::from_str("a"),
        );
        edit_menu.addItem(&select_all);
    }
    edit_menu_item.setSubmenu(Some(&edit_menu));
    main_menu.addItem(&edit_menu_item);

    app.setMainMenu(Some(&main_menu));
}
