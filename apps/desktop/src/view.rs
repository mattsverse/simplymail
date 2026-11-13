//! What the window shows: rune-kit's app shell, with accounts in the sidebar and mail, a message
//! being written, the add-account form or the settings in the main pane.

use jiff::Zoned;
use rune::{
    Block, Color, Cx, El, Icon, Mark, ScrollSpec, TextEdit, TextStyle, Tx, col, el, row, text,
};
use rune_kit::shell::{RowStatus, Sidebar, SidebarRow, SplitView, Toolbar};
use rune_kit::{
    Button, ButtonVariant, Divider, IconButton, IconButtonVariant, Kit, Markdown, MarkdownStyle,
    Menu, MenuItem, Placement, SegmentedControl, TextArea, TextInput, Toast, ToastRegion, glyphs,
    root,
};
use simplymail_app::{POLL_INTERVALS, Theme};
use simplymail_core::account::{Account, Security};
use simplymail_core::message::{Address, Mailbox, Role};
use simplymail_html::compat::{self, Client, Provider};
use simplymail_mail::imap::Status;

use crate::compose::{ComposeForm, Mode};
use crate::form::{AccountForm, SECURITY, Step, security_index};
use crate::state::{INBOX as INBOX_NAME, Mail, Reader, Showing};

static INBOX: Icon = Icon {
    name: "simplymail-inbox",
    view: 16.0,
    body: r#"<path d="M2.75 9.25L4.5 3.75H11.5L13.25 9.25V12.25H2.75Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M2.75 9.25H5.75L6.5 10.5H9.5L10.25 9.25H13.25" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};
/// The open message's subject.
const SUBJECT: TextStyle = TextStyle::new(20.0, 600.0).lh(26.0);

static BACK: Icon = Icon {
    name: "simplymail-back",
    view: 16.0,
    body: r#"<path d="M10 3.5L5.5 8L10 12.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static SENT: Icon = Icon {
    name: "simplymail-sent",
    view: 16.0,
    body: r#"<path d="M2.5 7.75L13.5 2.75L9.75 13.25L7.5 8.5Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M7.5 8.5L13.5 2.75" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};
static MORE: Icon = Icon {
    name: "simplymail-more",
    view: 16.0,
    body: r#"<circle cx="3.5" cy="8" r="1.1" fill="currentColor"/><circle cx="8" cy="8" r="1.1" fill="currentColor"/><circle cx="12.5" cy="8" r="1.1" fill="currentColor"/>"#,
};
static FOLDER: Icon = Icon {
    name: "simplymail-folder",
    view: 16.0,
    body: r#"<path d="M2.5 4.25C2.5 3.84 2.84 3.5 3.25 3.5H6.25L7.5 5H12.75C13.16 5 13.5 5.34 13.5 5.75V11.75C13.5 12.16 13.16 12.5 12.75 12.5H3.25C2.84 12.5 2.5 12.16 2.5 11.75Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};
static GEAR: Icon = Icon {
    name: "simplymail-gear",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="2" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M8 1.75V3.5M8 12.5V14.25M1.75 8H3.5M12.5 8H14.25M3.58 3.58L4.82 4.82M11.18 11.18L12.42 12.42M3.58 12.42L4.82 11.18M11.18 4.82L12.42 3.58" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/><circle cx="8" cy="8" r="4.5" stroke="currentColor" stroke-width="1.2" fill="none"/>"#,
};
static PLUS: Icon = Icon {
    name: "simplymail-plus",
    view: 16.0,
    body: r#"<path d="M8 3.5V12.5M3.5 8H12.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

pub(crate) fn view(s: &Mail, cx: &mut Cx<Mail>) -> El {
    let k = Kit::neutral(s.is_dark(cx.system_dark()));
    if let Err(error) = &s.services {
        return unavailable(error, &k);
    }
    let main = if s.settings_open {
        settings(s, &k)
    } else if s.form.open {
        form(&s.form, &k)
    } else if s.compose.open {
        compose(s, &s.compose, &k)
    } else if let Some(reader) = &s.reader {
        message(reader, &k)
    } else {
        inbox(s, &k)
    };
    let mut toolbar = Toolbar::new().title(title(s));
    let mut trailing = row().gap(12.0).center_y();
    let mut layer = Vec::new();
    if let Some(notice) = &s.notice {
        layer.push(
            ToastRegion::new()
                .toast(
                    Toast::new("notice", notice.clone())
                        .on_close(Mail::dismiss_notice)
                        .build(&k),
                )
                .build(&k),
        );
    }
    let browsing = !s.form.open && !s.compose.open && !s.settings_open;
    if let (Some(reader), true) = (&s.reader, browsing) {
        toolbar = toolbar.leading(
            Button::new("Back")
                .icon(&BACK)
                .variant(ButtonVariant::Ghost)
                .help("Back to the list (Esc)")
                .on_press(Mail::close_reader)
                .build(&k)
                .key("back"),
        );
        if let Some(Ok(_)) = reader.body {
            trailing = trailing.child(
                Button::new("Reply")
                    .variant(ButtonVariant::Ghost)
                    .shortcut("⌘R")
                    .on_press(|s: &mut Mail| s.reply(false))
                    .build(&k)
                    .key("reply"),
            );
        }
    }
    if browsing && !s.accounts.is_empty() {
        trailing = trailing.child(actions(s, &k, &mut layer));
    }
    let toolbar = toolbar.trailing(trailing);
    let window = SplitView::new(main)
        .sidebar(&s.sidebar, |s: &mut Mail| &mut s.sidebar, sidebar(s, &k))
        .toolbar(toolbar)
        .titlebar_inset(cx.titlebar_inset())
        .frame_inset(8.0)
        .toolbar_in_panel()
        .sidebar_padding(18.0, 10.0)
        .reduced_motion(cx.is_reduced_motion())
        .build(&k);
    root(window, layer)
}

/// The ⋯ menu of everything but Reply, which keeps a button of its own.
fn actions(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> El {
    let close = |s: &mut Mail| s.actions_open = false;
    let mut menu = Menu::new("actions")
        .trigger(
            IconButton::new(&MORE)
                .variant(IconButtonVariant::Ghost)
                .label("More actions")
                .help("More actions")
                .expanded(s.actions_open)
                .on_press(|s: &mut Mail| s.actions_open = !s.actions_open)
                .build(k),
        )
        .placement(Placement::BottomEnd)
        .min_width(200.0)
        .open(s.actions_open)
        .on_close(close)
        .item(
            MenuItem::new("New message")
                .shortcut("⌘N")
                .on_select(|s: &mut Mail| {
                    s.actions_open = false;
                    s.compose();
                }),
        );
    if let Some(reader) = &s.reader {
        let downloaded = matches!(reader.body, Some(Ok(_)));
        let seen = if reader.message.envelope.seen {
            "Mark as unread"
        } else {
            "Mark as read"
        };
        menu = menu
            .separator()
            .item(
                MenuItem::new("Reply all")
                    .shortcut("⇧⌘R")
                    .disabled(!downloaded)
                    .on_select(|s: &mut Mail| {
                        s.actions_open = false;
                        s.reply(true);
                    }),
            )
            .item(
                MenuItem::new(seen)
                    .shortcut("⇧⌘U")
                    .on_select(|s: &mut Mail| {
                        s.actions_open = false;
                        s.toggle_seen();
                    }),
            );
    }
    menu.build(k, layer)
}

fn title(s: &Mail) -> String {
    if s.settings_open {
        return "Settings".into();
    }
    if s.form.open {
        return match s.form.step {
            Step::Choose => "Add an account",
            Step::Google => "Sign in with Google",
            Step::Servers => "Other mail (IMAP and SMTP)",
        }
        .into();
    }
    if s.compose.open {
        return if s.compose.is_reply() {
            "Reply"
        } else {
            "New message"
        }
        .into();
    }
    match &s.showing {
        Showing::All(Role::Sent) => "Sent".into(),
        Showing::All(_) => "All inboxes".into(),
        Showing::Mailbox { account, name } if name == INBOX_NAME => s.label(account).to_owned(),
        Showing::Mailbox { account, name } => {
            let label = s
                .mailboxes
                .get(account)
                .and_then(|list| list.iter().find(|m| m.name == *name))
                .map_or_else(|| name.clone(), Mailbox::label);
            format!("{label} · {}", s.label(account))
        }
    }
}

fn unavailable(error: &str, k: &Kit) -> El {
    col()
        .key("unavailable")
        .w_full()
        .h_full()
        .center()
        .gap(8.0)
        .fill(k.c.bg_base)
        .window_drag()
        .child(
            text("SimplyMail couldn't start")
                .font(k.ty.body_medium)
                .color(k.c.text_base),
        )
        .child(
            text(error.to_owned())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap()
                .max_w(520.0),
        )
}

fn sidebar(s: &Mail, k: &Kit) -> El {
    let badge = |n: usize| (n > 0).then(|| n.to_string());
    let showing = |role| !s.form.open && !s.settings_open && s.showing == Showing::All(role);
    let mut all = SidebarRow::new("All inboxes")
        .icon(&INBOX)
        .height(32.0)
        .selected(showing(Role::Inbox))
        .on_select(|s: &mut Mail, _| s.show(Showing::All(Role::Inbox)));
    if let Some(n) = badge(s.unread(None)) {
        all = all.badge(n);
    }
    let sent = SidebarRow::new("Sent")
        .icon(&SENT)
        .height(32.0)
        .selected(showing(Role::Sent))
        .on_select(|s: &mut Mail, _| s.show(Showing::All(Role::Sent)));
    let folders = col()
        .gap(4.0)
        .child(all.build(k).key("all"))
        .child(sent.build(k).key("sent"))
        .child(col().pt(8.0).child(Divider::horizontal().build(k)));
    let mut sidebar = Sidebar::new()
        .label("Accounts")
        .inset(0.0)
        .header_gap(4.0)
        .header_padding(0.0, 12.0)
        .row_gap(4.0)
        .header(folders)
        .footer(
            col()
                .gap(4.0)
                .child(
                    SidebarRow::new("Add account")
                        .icon(&PLUS)
                        .height(32.0)
                        .selected(s.form.open && !s.settings_open)
                        .on_select(|s: &mut Mail, _| s.open_form())
                        .build(k)
                        .key("add-account"),
                )
                .child(
                    SidebarRow::new("Settings")
                        .icon(&GEAR)
                        .height(32.0)
                        .selected(s.settings_open)
                        .on_select(|s: &mut Mail, _| s.open_settings())
                        .build(k)
                        .key("settings"),
                ),
        );
    for account in &s.accounts {
        sidebar = sidebar.child(account_row(s, account, k));
        if !s.collapsed.contains(&account.id) {
            let listed = s.mailboxes.get(&account.id).map_or(&[][..], Vec::as_slice);
            for mailbox in listed.iter().filter(|m| m.role != Role::Inbox) {
                sidebar = sidebar.child(mailbox_row(s, &account.id, mailbox, k));
            }
        }
    }
    sidebar.build(k)
}

/// One of an account's mailboxes other than its inbox, under the account.
fn mailbox_row(s: &Mail, account: &str, mailbox: &Mailbox, k: &Kit) -> El {
    let showing = Showing::Mailbox {
        account: account.to_owned(),
        name: mailbox.name.clone(),
    };
    let selected = !s.form.open && !s.settings_open && s.showing == showing;
    SidebarRow::new(mailbox.label())
        .icon(if mailbox.role == Role::Sent {
            &SENT
        } else {
            &FOLDER
        })
        .depth(1)
        .selected(selected)
        .on_select(move |s: &mut Mail, _| s.show(showing.clone()))
        .build(k)
        .key(format!("mailbox-{account}-{}", mailbox.name))
}

fn account_row(s: &Mail, account: &Account, k: &Kit) -> El {
    let status = s.statuses.get(&account.id);
    let confirming = s.confirm_remove.as_deref() == Some(account.id.as_str());
    let (subtitle, row_status) = if confirming {
        ("Press × again to remove".to_owned(), None)
    } else {
        match status {
            None | Some(Ok(Status::Connecting)) => {
                ("Connecting…".to_owned(), Some(RowStatus::Busy))
            }
            Some(Ok(Status::Push)) => (account.id.clone(), None),
            Some(Ok(Status::Polling(every))) => {
                (format!("Checking every {}s", every.as_secs()), None)
            }
            Some(Ok(Status::Failed { error, retry_in })) => (
                format!("{error} · retrying in {}s", retry_in.as_secs()),
                Some(RowStatus::Attention),
            ),
            Some(Err(error)) => (error.clone(), Some(RowStatus::Attention)),
        }
    };
    let select = account.id.clone();
    let remove = account.id.clone();
    let toggle = account.id.clone();
    let expanded = !s.collapsed.contains(&account.id);
    let disclosure = IconButton::new(if expanded {
        &glyphs::TRIANGLE_DOWN
    } else {
        &glyphs::TRIANGLE_RIGHT
    })
    .variant(IconButtonVariant::GhostMuted)
    .label(if expanded {
        "Hide mailboxes"
    } else {
        "Show mailboxes"
    })
    .expanded(expanded)
    .on_press(move |s: &mut Mail| s.toggle_mailboxes(&toggle))
    .build(k);
    let mut r = SidebarRow::new(account.label().to_owned())
        .height(45.0)
        .avatar(avatar(account.label(), k))
        .subtitle(subtitle)
        .selected(!s.form.open && !s.settings_open && s.showing.is(&account.id, INBOX_NAME))
        .trailing(disclosure)
        // The account itself stands for its inbox; its other mailboxes are the rows below.
        .on_select(move |s: &mut Mail, _| s.show(Showing::inbox(&select)))
        .on_close(move |s: &mut Mail, _| s.remove(&remove));
    if confirming {
        r = r.subtitle_color(k.c.danger.fg);
    }
    if let Some(row_status) = row_status {
        r = r.status(row_status);
    }
    let unread = s.unread(Some(&account.id));
    if unread > 0 {
        r = r.badge(unread.to_string());
    }
    r.build(k).key(format!("account-{}", account.id))
}

/// A rounded square with the account's initial.
fn avatar(name: &str, k: &Kit) -> El {
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    row()
        .size(16.0, 16.0)
        .radius(4.0)
        .center()
        .fill(k.c.bg_accent)
        .child(text(initial).font(k.ty.small_medium).color(Color::WHITE))
}

fn inbox(s: &Mail, k: &Kit) -> El {
    if s.shown().next().is_none() {
        let hint = if s.accounts.is_empty() {
            "Add an account to get started."
        } else if s.showing_sent() {
            "No sent mail yet."
        } else if matches!(&s.showing, Showing::Mailbox { name, .. } if name != INBOX_NAME) {
            "Nothing here yet. Other mailboxes are checked every few minutes."
        } else {
            "No mail yet. New messages appear here as they arrive."
        };
        return col()
            .key("empty")
            .w_full()
            .grow(1.0)
            .center()
            .child(text(hint).font(k.ty.ui).color(k.c.text_muted));
    }
    let now = Zoned::now();
    // Mail from several accounts says whose it is.
    let multiple_accounts = s.showing.account().is_none() && s.accounts.len() > 1;
    let sent = s.showing_sent();
    let mut list = col().key("messages").w_full();
    for m in s.shown() {
        let e = &m.envelope;
        let strong = if e.seen { k.ty.ui } else { k.ty.ui_medium };
        let (account, mailbox, uid) = (m.account.clone(), m.mailbox.clone(), e.uid);
        let mut line = row()
            .key(format!("msg-{}-{}-{}", m.account, m.mailbox, e.uid))
            .on_press(move |s: &mut Mail| s.open_message(&account, &mailbox, uid))
            .w_full()
            .h(40.0)
            .px(20.0)
            .gap(14.0)
            .center_y()
            .hover(k.c.hover)
            .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
            .child(dot(if e.seen {
                Color::TRANSPARENT
            } else {
                k.c.text_accent
            }))
            .child(
                text(if sent {
                    format!("To: {}", simplymail_app::recipients(&e.to))
                } else {
                    e.from.label().to_owned()
                })
                .font(strong)
                .color(k.c.text_base)
                .truncate()
                .w(190.0),
            )
            .child(
                text(e.subject.clone().unwrap_or_else(|| "(no subject)".into()))
                    .font(strong)
                    .color(if e.seen {
                        k.c.text_muted
                    } else {
                        k.c.text_base
                    })
                    .truncate()
                    .grow(1.0)
                    .min_w(0.0),
            );
        if multiple_accounts {
            line = line.child(
                text(s.label(&m.account).to_owned())
                    .font(k.ty.small)
                    .color(k.c.text_faint)
                    .truncate()
                    .w(140.0),
            );
        }
        list = list.child(
            line.child(
                row().w(64.0).justify_end().child(
                    text(simplymail_app::format_date(e.date, &now))
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                ),
            ),
        );
    }
    list.scroll(ScrollSpec {
        stick_bottom: false,
    })
    .grow(1.0)
    .min_h(0.0)
}

/// The open message: its headers, then its text.
fn message(r: &Reader, k: &Kit) -> El {
    let e = &r.message.envelope;
    let subject = e.subject.clone().unwrap_or_else(|| "(no subject)".into());
    let mut header = col()
        .gap(6.0)
        .child(
            text(subject)
                .key("subject")
                .font(SUBJECT)
                .color(k.c.text_base)
                .wrap()
                .selectable(true),
        )
        .child(header_line(k, "From", std::slice::from_ref(&e.from)));
    if let Some(Ok(body)) = &r.body {
        if !body.to.is_empty() {
            header = header.child(header_line(k, "To", &body.to));
        }
        if !body.cc.is_empty() {
            header = header.child(header_line(k, "Cc", &body.cc));
        }
    }
    if let Some(date) = long_date(e.date) {
        header = header.child(text(date).font(k.ty.small).color(k.c.text_muted));
    }
    let content = match &r.body {
        None => text("Downloading…")
            .key("downloading")
            .font(k.ty.ui)
            .color(k.c.text_muted),
        Some(Err(error)) => col()
            .key("open-failed")
            .gap(10.0)
            .child(
                text(format!("Couldn't download this message: {error}"))
                    .font(k.ty.ui)
                    .color(k.c.danger.fg)
                    .wrap(),
            )
            .child(Button::new("Try again").on_press(Mail::retry_open).build(k)),
        Some(Ok(body)) => {
            let mut content = col().gap(16.0);
            if !body.attachments.is_empty() {
                content = content.child(
                    text(format!("Attachments: {}", body.attachments.join(", ")))
                        .key("attachments")
                        .font(k.ty.small)
                        .color(k.c.text_muted)
                        .wrap(),
                );
            }
            if let Some(markdown) = &r.markdown {
                return reader_frame(
                    header,
                    content.child(
                        Markdown::new(markdown.clone())
                            .selectable(true)
                            .on_link_tx(|s: &mut Mail, url: &str, tx: &mut Tx| s.open_link(url, tx))
                            .build(k)
                            .key("body"),
                    ),
                    k,
                );
            }
            let shown = if body.text.is_empty() {
                text("This message has no text.").color(k.c.text_muted)
            } else {
                text(body.text.clone()).color(k.c.text_base)
            };
            content.child(shown.key("body").font(k.ty.body).wrap().selectable(true))
        }
    };
    reader_frame(header, content, k)
}

/// The reader's scrolling column: the headers, a rule, then the content.
fn reader_frame(header: El, content: El, k: &Kit) -> El {
    col()
        .key("reader")
        .w_full()
        .child(
            col()
                .w_full()
                .max_w(760.0)
                .padding(28.0)
                .gap(20.0)
                .child(header)
                .child(el().w_full().h(1.0).fill(k.c.border_weak))
                .child(content),
        )
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
}

/// The message being written: who it's from and to, its subject and text, and Send.
fn compose(s: &Mail, c: &ComposeForm, k: &Kit) -> El {
    let mut fields = col()
        .w_full()
        .max_w(760.0)
        .padding(24.0)
        .gap(12.0)
        .child(field(k, "From", from_picker(s, c, k)))
        .child(field(
            k,
            "To",
            compose_input(
                k,
                c,
                "To",
                "ada@example.com, Bob <bob@example.com>",
                &c.to,
                |s| &mut s.compose.to,
            ),
        ))
        .child(
            row()
                .gap(12.0)
                .child(field(
                    k,
                    "Cc",
                    compose_input(k, c, "Cc", "", &c.cc, |s| &mut s.compose.cc),
                ))
                .child(field(
                    k,
                    "Bcc",
                    compose_input(k, c, "Bcc", "", &c.bcc, |s| &mut s.compose.bcc),
                )),
        )
        .child(field(
            k,
            "Subject",
            compose_input(k, c, "Subject", "", &c.subject, |s| &mut s.compose.subject),
        ))
        .child(editor(c, k));
    if let Some(quote) = quote_view(c, k) {
        fields = fields.child(quote);
    }
    fields = fields.child(compatibility(c, k));
    if let Some(error) = &c.error {
        fields = fields.child(
            text(error.clone())
                .key("compose-error")
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap(),
        );
    }
    col()
        .key("compose")
        .w_full()
        .child(fields.child(compose_actions(c, k)))
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
}

/// Who a message is sent from: a picker when there's more than one account.
fn from_picker(s: &Mail, c: &ComposeForm, k: &Kit) -> El {
    if s.accounts.len() > 1 {
        let labels: Vec<&str> = s.accounts.iter().map(Account::label).collect();
        let current = s.accounts.iter().position(|a| a.id == c.account);
        SegmentedControl::new(&labels, current)
            .label("From")
            .fit()
            .on_pick(|s: &mut Mail, i, _| {
                if let Some(a) = s.accounts.get(i) {
                    s.compose.account.clone_from(&a.id);
                }
            })
            .build(k)
    } else {
        text(s.label(&c.account).to_owned())
            .font(k.ty.ui)
            .color(k.c.text_base)
    }
}

fn compose_actions(c: &ComposeForm, k: &Kit) -> El {
    let (label, variant) = if c.sending {
        ("Sending…", ButtonVariant::Loading)
    } else {
        ("Send", ButtonVariant::Contrast)
    };
    row()
        .gap(8.0)
        .center_y()
        .child(
            Button::new(label)
                .variant(variant)
                .disabled(c.sending)
                .shortcut("⌘↩")
                .on_press(Mail::send)
                .build(k),
        )
        .child(
            Button::new("Discard")
                .variant(ButtonVariant::Ghost)
                .on_press(Mail::discard)
                .build(k),
        )
}

/// One of the compose form's one-line fields, filling its column.
fn compose_input(
    k: &Kit,
    c: &ComposeForm,
    label: &str,
    placeholder: &str,
    edit: &TextEdit,
    get: fn(&mut Mail) -> &mut TextEdit,
) -> El {
    TextInput::new(edit)
        .label(label)
        .placeholder(placeholder)
        .width(None)
        .disabled(c.sending)
        .on_edit(get)
        .build(k)
}

/// "From: Ada <ada@example.com>", for a header line.
fn header_line(k: &Kit, name: &str, addresses: &[Address]) -> El {
    let list = addresses
        .iter()
        .map(Address::full)
        .collect::<Vec<_>>()
        .join(", ");
    row()
        .gap(6.0)
        .w_full()
        .child(
            text(format!("{name}:"))
                .font(k.ty.small_medium)
                .color(k.c.text_muted),
        )
        .child(
            text(list)
                .font(k.ty.small)
                .color(k.c.text_base)
                .wrap()
                .grow(1.0)
                .min_w(0.0)
                .selectable(true),
        )
}

/// The full date and time, in the local time zone.
fn long_date(unix: Option<i64>) -> Option<String> {
    let at = jiff::Timestamp::from_second(unix?).ok()?;
    Some(
        at.to_zoned(jiff::tz::TimeZone::system())
            .strftime("%A %-d %B %Y at %H:%M")
            .to_string(),
    )
}

fn form(f: &AccountForm, k: &Kit) -> El {
    let page = match f.step {
        Step::Choose => choose(k),
        Step::Google => google_form(f, k),
        Step::Servers => servers_form(f, k),
    };
    col()
        .key("form-scroll")
        .w_full()
        .child(page)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
}

/// One screen of the add-account form. The toolbar names it.
fn form_page(key: &str) -> El {
    col().key(key).w_full().max_w(560.0).padding(24.0).gap(16.0)
}

/// The form's error, if there is one, under its fields.
fn with_error(page: El, f: &AccountForm, k: &Kit) -> El {
    match &f.error {
        Some(error) => page.child(
            text(error.clone())
                .key("form-error")
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap(),
        ),
        None => page,
    }
}

/// The first screen: how to connect the account.
fn choose(k: &Kit) -> El {
    let option = |key: &str, title: &str, about: &str, action: &str, step: Step| {
        col()
            .key(key)
            .w_full()
            .gap(8.0)
            .padding(16.0)
            .radius(8.0)
            .border(1.0, k.c.border_weak)
            .child(
                text(title.to_owned())
                    .font(k.ty.ui_medium)
                    .color(k.c.text_base),
            )
            .child(
                text(about.to_owned())
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .wrap(),
            )
            .child(
                row().child(
                    Button::new(action)
                        .variant(ButtonVariant::Contrast)
                        .on_press(move |s: &mut Mail| s.form_step(step))
                        .build(k),
                ),
            )
    };
    form_page("form-choose")
        .child(option(
            "choose-google",
            "Google",
            "Gmail and Google Workspace. Sign in in your browser; no app password needed.",
            "Sign in with Google",
            Step::Google,
        ))
        .child(option(
            "choose-servers",
            "Other mail (IMAP and SMTP)",
            "Any provider: fill in its incoming and outgoing servers and your password.",
            "Set up IMAP and SMTP",
            Step::Servers,
        ))
        .child(
            row().child(
                Button::new("Cancel")
                    .variant(ButtonVariant::Ghost)
                    .on_press(|s: &mut Mail| s.form = AccountForm::default())
                    .build(k),
            ),
        )
}

/// Signing in with Google, or the sign-in under way.
fn google_form(f: &AccountForm, k: &Kit) -> El {
    let mut page = form_page("form-google")
        .child(
            text(
                "Google's sign-in page opens in your browser. SimplyMail only gets access to \
                 your mail, and keeps the sign-in in your keyring."
                    .to_owned(),
            )
            .font(k.ty.small)
            .color(k.c.text_muted)
            .wrap(),
        )
        .child(
            row()
                .gap(12.0)
                .child(field(
                    k,
                    "Gmail address (optional)",
                    input(f, "Gmail address", "you@gmail.com", &f.email, |s| {
                        &mut s.form.email
                    })
                    .build(k),
                ))
                .child(field(
                    k,
                    "Your name (optional)",
                    input(f, "Your name", "Ada Lovelace", &f.name, |s| {
                        &mut s.form.name
                    })
                    .build(k),
                )),
        );
    if let Some(url) = f.google.as_ref().and_then(|g| g.url.as_ref()) {
        page = page.child(
            Markdown::new(format!(
                "Finish signing in in your browser. If it didn't open, \
                 [open Google's sign-in page]({url})."
            ))
            .style(
                MarkdownStyle::from_kit(k)
                    .body_font(k.ty.small)
                    .text(k.c.text_muted),
            )
            .on_link_tx(|s: &mut Mail, url: &str, tx: &mut Tx| s.open_link(url, tx))
            .build(k)
            .key("google-url"),
        );
    }
    let actions = if f.google.is_some() {
        row()
            .gap(8.0)
            .child(
                Button::new("Waiting for Google…")
                    .variant(ButtonVariant::Loading)
                    .disabled(true)
                    .build(k),
            )
            .child(
                Button::new("Cancel")
                    .variant(ButtonVariant::Ghost)
                    .on_press(Mail::cancel_sign_in)
                    .build(k),
            )
    } else {
        row()
            .gap(8.0)
            .child(
                Button::new("Sign in with Google")
                    .variant(ButtonVariant::Contrast)
                    .on_press_tx(Mail::submit_form)
                    .build(k),
            )
            .child(back(k, false))
    };
    with_error(page, f, k).child(actions)
}

/// Typing in IMAP and SMTP servers and a password.
fn servers_form(f: &AccountForm, k: &Kit) -> El {
    let imap_port = f.imap_security.default_imap_port().to_string();
    let smtp_port = f.smtp_security.default_smtp_port().to_string();
    let smtp_host = f.smtp_guess().unwrap_or_else(|| "smtp.example.com".into());
    let fields = form_page("form")
        .child(
            row()
                .gap(12.0)
                .child(field(
                    k,
                    "Email",
                    input(f, "Email", "you@example.com", &f.email, |s| {
                        &mut s.form.email
                    })
                    .build(k),
                ))
                .child(field(
                    k,
                    "Your name (optional)",
                    input(f, "Your name", "Ada Lovelace", &f.name, |s| {
                        &mut s.form.name
                    })
                    .build(k),
                )),
        )
        .child(
            row()
                .gap(12.0)
                .child(field(
                    k,
                    "Password or app password",
                    input(f, "Password", "", &f.password, |s| &mut s.form.password)
                        .secure()
                        .build(k),
                ))
                .child(field(
                    k,
                    "User name (if not the email)",
                    input(f, "User name", "", &f.username, |s| &mut s.form.username).build(k),
                )),
        )
        .child(server(
            k,
            "Incoming mail (IMAP)",
            input(f, "IMAP server", "imap.example.com", &f.imap_host, |s| {
                &mut s.form.imap_host
            })
            .build(k),
            input(f, "IMAP port", &imap_port, &f.imap_port, |s| {
                &mut s.form.imap_port
            })
            .build(k),
            f.imap_security,
            |s, security| s.form.imap_security = security,
            None,
        ))
        .child(server(
            k,
            "Outgoing mail (SMTP)",
            input(f, "SMTP server", &smtp_host, &f.smtp_host, |s| {
                &mut s.form.smtp_host
            })
            .build(k),
            input(f, "SMTP port", &smtp_port, &f.smtp_port, |s| {
                &mut s.form.smtp_port
            })
            .build(k),
            f.smtp_security,
            |s, security| s.form.smtp_security = security,
            Some(smtp_login(f, k)),
        ));
    with_error(fields, f, k).child(form_actions(f, k))
}

/// Back to choosing how to connect.
fn back(k: &Kit, disabled: bool) -> El {
    Button::new("Back")
        .variant(ButtonVariant::Ghost)
        .disabled(disabled)
        .on_press(|s: &mut Mail| s.form_step(Step::Choose))
        .build(k)
}

/// The SMTP user name and password, for servers that want different ones than IMAP.
fn smtp_login(f: &AccountForm, k: &Kit) -> El {
    row()
        .gap(12.0)
        .child(field(
            k,
            "User name (if different)",
            input(f, "SMTP user name", "", &f.smtp_username, |s| {
                &mut s.form.smtp_username
            })
            .build(k),
        ))
        .child(field(
            k,
            "Password (if different)",
            input(f, "SMTP password", "", &f.smtp_password, |s| {
                &mut s.form.smtp_password
            })
            .secure()
            .build(k),
        ))
}

fn form_actions(f: &AccountForm, k: &Kit) -> El {
    let (label, variant) = if f.checking {
        ("Checking the login…", ButtonVariant::Loading)
    } else {
        ("Add account", ButtonVariant::Contrast)
    };
    row()
        .gap(8.0)
        .child(
            Button::new(label)
                .variant(variant)
                .disabled(f.checking)
                .on_press_tx(Mail::submit_form)
                .build(k),
        )
        .child(back(k, f.checking))
}

fn server(
    k: &Kit,
    title: &str,
    host: El,
    port: El,
    security: Security,
    set: fn(&mut Mail, Security),
    login: Option<El>,
) -> El {
    let labels: Vec<&str> = SECURITY.iter().map(|(label, _)| *label).collect();
    let mut section = col()
        .gap(8.0)
        .child(
            text(title.to_owned())
                .font(k.ty.ui_medium)
                .color(k.c.text_base),
        )
        .child(
            row()
                .gap(12.0)
                .child(field(k, "Server", host).grow(3.0))
                .child(field(k, "Port", port).grow(1.0)),
        )
        .child(
            SegmentedControl::new(&labels, Some(security_index(security)))
                .label(format!("{title} security"))
                .fit()
                .on_pick(move |s: &mut Mail, i, _| {
                    if let Some((_, security)) = SECURITY.get(i) {
                        set(s, *security);
                    }
                })
                .build(k),
        );
    if let Some(login) = login {
        section = section.child(login);
    }
    section
}

/// One of the form's text fields, filling its column. `label` is its accessible name.
fn input(
    f: &AccountForm,
    label: &str,
    placeholder: &str,
    edit: &TextEdit,
    get: fn(&mut Mail) -> &mut TextEdit,
) -> TextInput {
    TextInput::new(edit)
        .label(label)
        .placeholder(placeholder)
        .width(None)
        .disabled(f.checking)
        .on_edit(get)
        .on_submit(Mail::submit_form)
}

/// A visible label over a control.
fn field(k: &Kit, label: &str, input: El) -> El {
    col()
        .gap(5.0)
        .grow(1.0)
        .min_w(0.0)
        .child(
            text(label.to_owned())
                .font(k.ty.small_medium)
                .color(k.c.text_muted),
        )
        .child(input)
}

/// The settings: the theme, how often mail is checked, and the accounts, each removable.
fn settings(s: &Mail, k: &Kit) -> El {
    let themes: Vec<&str> = Theme::ALL.iter().map(|t| t.label()).collect();
    let theme = SegmentedControl::new(
        &themes,
        Theme::ALL.iter().position(|t| *t == s.settings.theme),
    )
    .label("Theme")
    .fit()
    .on_pick(|s: &mut Mail, i, _| {
        if let Some(theme) = Theme::ALL.get(i) {
            s.set_theme(*theme);
        }
    })
    .build(k)
    .key("theme");
    let intervals: Vec<String> = POLL_INTERVALS.iter().map(|d| interval_label(*d)).collect();
    let interval = SegmentedControl::new(
        &intervals,
        POLL_INTERVALS
            .iter()
            .position(|d| *d == s.settings.poll_interval),
    )
    .label("Check for new mail every")
    .fit()
    .on_pick(|s: &mut Mail, i, _| {
        if let Some(every) = POLL_INTERVALS.get(i) {
            s.set_poll_interval(*every);
        }
    })
    .build(k)
    .key("poll-interval");
    let mut accounts = col().w_full().gap(8.0);
    if s.accounts.is_empty() {
        accounts = accounts.child(
            text("No accounts yet.")
                .font(k.ty.small)
                .color(k.c.text_muted),
        );
    }
    for account in &s.accounts {
        accounts = accounts.child(settings_account(s, account, k));
    }
    let page = col()
        .key("settings")
        .w_full()
        .max_w(560.0)
        .padding(24.0)
        .gap(24.0)
        .child(setting_section(k, "Appearance", None, row().child(theme)))
        .child(setting_section(
            k,
            "Checking for mail",
            Some(
                "Only for servers that can't tell SimplyMail about new mail as it arrives (IMAP \
                 IDLE). Changing it reconnects every account.",
            ),
            row().child(interval),
        ))
        .child(setting_section(k, "Accounts", None, accounts));
    col()
        .key("settings-scroll")
        .w_full()
        .child(page)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
}

/// A titled group of settings, with an optional explanation under its control.
fn setting_section(k: &Kit, title: &str, about: Option<&str>, control: El) -> El {
    let mut section = col().w_full().gap(8.0).child(
        text(title.to_owned())
            .font(k.ty.ui_medium)
            .color(k.c.text_base),
    );
    section = section.child(control);
    if let Some(about) = about {
        section = section.child(
            text(about.to_owned())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    section
}

/// An account in the settings, with its Remove button, which asks for a second press.
fn settings_account(s: &Mail, account: &Account, k: &Kit) -> El {
    let confirming = s.confirm_remove.as_deref() == Some(account.id.as_str());
    let remove = account.id.clone();
    let mut actions = row().gap(8.0).center_y();
    if confirming {
        actions = actions.child(
            Button::new("Cancel")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut Mail| s.confirm_remove = None)
                .build(k),
        );
    }
    actions = actions.child(
        Button::new(if confirming {
            "Remove for good"
        } else {
            "Remove"
        })
        .variant(if confirming {
            ButtonVariant::Danger
        } else {
            ButtonVariant::Neutral
        })
        .on_press(move |s: &mut Mail| s.remove(&remove))
        .build(k),
    );
    let subtitle = if confirming {
        text(
            "Its mail is deleted from this computer, and its password or sign-in forgotten. \
              Nothing changes on the server.",
        )
        .color(k.c.danger.fg)
        .wrap()
    } else {
        text(account.id.clone()).color(k.c.text_muted).truncate()
    };
    row()
        .key(format!("settings-account-{}", account.id))
        .w_full()
        .gap(12.0)
        .padding(12.0)
        .radius(8.0)
        .border(1.0, k.c.border_weak)
        .center_y()
        .child(avatar(account.label(), k))
        .child(
            col()
                .gap(2.0)
                .grow(1.0)
                .min_w(0.0)
                .child(
                    text(account.label().to_owned())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base)
                        .truncate(),
                )
                .child(subtitle.font(k.ty.small)),
        )
        .child(actions)
}

/// "30 s", "1 min", "15 min".
fn interval_label(every: std::time::Duration) -> String {
    let secs = every.as_secs();
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min", secs / 60)
    }
}

fn dot(color: Color) -> El {
    el().size(8.0, 8.0).radius(4.0).fill(color)
}

static BOLD: Icon = Icon {
    name: "simplymail-bold",
    view: 16.0,
    body: r#"<path d="M5 3H8.6C9.98 3 11.1 4.12 11.1 5.5C11.1 6.88 9.98 8 8.6 8H5ZM5 8H9.1C10.48 8 11.6 9.12 11.6 10.5C11.6 11.88 10.48 13 9.1 13H5Z" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" fill="none"/>"#,
};
static ITALIC: Icon = Icon {
    name: "simplymail-italic",
    view: 16.0,
    body: r#"<path d="M7.5 3H12M4 13H8.5M9.75 3L6.25 13" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#,
};
static UNDERLINE: Icon = Icon {
    name: "simplymail-underline",
    view: 16.0,
    body: r#"<path d="M4.75 2.75V7.25C4.75 9.05 6.2 10.5 8 10.5C9.8 10.5 11.25 9.05 11.25 7.25V2.75M3.75 13.25H12.25" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#,
};
static STRIKE: Icon = Icon {
    name: "simplymail-strike",
    view: 16.0,
    body: r#"<path d="M2.75 8H13.25M10.9 4.9C10.55 3.8 9.4 3 8 3C6.35 3 5.1 3.95 5.1 5.3C5.1 6.1 5.55 6.7 6.4 7.2M5.1 11C5.45 12.2 6.6 13 8.1 13C9.85 13 11.1 12 11.1 10.65C11.1 10.05 10.9 9.55 10.45 9.15" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#,
};
static CODE: Icon = Icon {
    name: "simplymail-code",
    view: 16.0,
    body: r#"<path d="M5.5 4.5L2 8L5.5 11.5M10.5 4.5L14 8L10.5 11.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static LINK: Icon = Icon {
    name: "simplymail-link",
    view: 16.0,
    body: r#"<path d="M6.9 9.1C7.6 9.9 8.8 10 9.6 9.3L11.9 7C12.75 6.15 12.75 4.8 11.9 3.95C11.05 3.1 9.7 3.1 8.85 3.95L8.2 4.6M9.1 6.9C8.4 6.1 7.2 6 6.4 6.7L4.1 9C3.25 9.85 3.25 11.2 4.1 12.05C4.95 12.9 6.3 12.9 7.15 12.05L7.8 11.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#,
};
static BULLETS: Icon = Icon {
    name: "simplymail-bullets",
    view: 16.0,
    body: r#"<circle cx="3.5" cy="4.5" r="1" fill="currentColor"/><circle cx="3.5" cy="8" r="1" fill="currentColor"/><circle cx="3.5" cy="11.5" r="1" fill="currentColor"/><path d="M6.5 4.5H13M6.5 8H13M6.5 11.5H13" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};
static NUMBERS: Icon = Icon {
    name: "simplymail-numbers",
    view: 16.0,
    body: r#"<path d="M2.6 3.3L3.5 2.75V6.25M2.4 9.6C2.55 9.15 2.9 8.9 3.35 8.9C3.85 8.9 4.2 9.25 4.2 9.65C4.2 10.4 2.4 11 2.4 12.25H4.3" stroke="currentColor" stroke-width="1" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M6.5 4.5H13M6.5 11H13" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};
static QUOTE: Icon = Icon {
    name: "simplymail-quote",
    view: 16.0,
    body: r#"<path d="M3.25 3.5V12.5" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/><path d="M6.5 5H13M6.5 8H13M6.5 11H11" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};
static CODE_BLOCK: Icon = Icon {
    name: "simplymail-code-block",
    view: 16.0,
    body: r#"<rect x="2.25" y="3" width="11.5" height="10" rx="1.75" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M6.75 6.5L5.25 8L6.75 9.5M9.25 6.5L10.75 8L9.25 9.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

/// The message's text: a formatting bar over the visual editor, or the HTML source.
fn editor(c: &ComposeForm, k: &Kit) -> El {
    let area = match c.mode {
        Mode::Visual => TextArea::new(&c.text)
            .label("Message")
            .auto_grow(12, 60)
            .width(None)
            .disabled(c.sending)
            .on_edit(|s: &mut Mail| &mut s.compose.text)
            .on_change(|s: &mut Mail, _| s.compose.refresh())
            .on_key(send_on_command_return)
            // Return breaks the line, as in any document; ⌘↩ sends.
            .on_submit(|s: &mut Mail, _| {
                s.compose.text.insert("\n");
                s.compose.refresh();
            })
            .build(k)
            .key("compose-text"),
        Mode::Source => TextArea::new(&c.source)
            .label("HTML source")
            .auto_grow(12, 60)
            .width(None)
            .disabled(c.sending)
            .on_edit(|s: &mut Mail| &mut s.compose.source)
            .on_change(|s: &mut Mail, _| s.compose.refresh())
            .on_key(send_on_command_return)
            // Return breaks the line, as in any document; ⌘↩ sends.
            .on_submit(|s: &mut Mail, _| {
                s.compose.source.insert("\n");
                s.compose.refresh();
            })
            .build(k)
            .key("compose-source"),
    };
    let mut editor = col().gap(8.0).w_full().child(format_bar(c, k));
    if c.confirm_visual {
        editor = editor.child(
            text(
                "This HTML uses markup the visual editor can't show, such as styles, tables or \
                 images. Choose Visual again to drop it, or keep editing the source.",
            )
            .key("confirm-visual")
            .font(k.ty.small)
            .color(k.c.warning.fg)
            .wrap(),
        );
    }
    if let Some(link) = &c.link {
        editor = editor.child(link_row(c, link, k));
    }
    editor.child(area)
}

/// ⌘↩ in the message sends it.
fn send_on_command_return(s: &mut Mail, ev: &rune::KeyEvent, _: &mut Tx) -> bool {
    if ev.key == rune::Key::Enter && ev.mods.primary() {
        s.send();
        return true;
    }
    false
}

/// Buttons for each kind of formatting, and the switch between visual and source editing.
fn format_bar(c: &ComposeForm, k: &Kit) -> El {
    let enabled = c.mode == Mode::Visual && !c.sending;
    let active = c.text.active_marks();
    let block = c.text.active_block();
    let mark = |icon: &'static Icon, label: &str, shortcut: Option<&str>, mark: Mark| {
        let on = enabled && active.iter().any(|m| m.same_kind(&mark));
        let mut button = tool(icon, label, on, enabled);
        if let Some(shortcut) = shortcut {
            button = button.shortcut(shortcut);
        }
        button
            .on_press(move |s: &mut Mail| s.compose.toggle_mark(mark.clone()))
            .build(k)
            .checked(on)
            .key(tool_key(label))
    };
    let block_button = |icon: &'static Icon, label: &str, kind: Block| {
        let on = enabled && block.same_kind(kind);
        tool(icon, label, on, enabled)
            .on_press(move |s: &mut Mail| s.compose.toggle_block(kind))
            .build(k)
            .checked(on)
            .key(tool_key(label))
    };
    let linked = enabled && c.text.active_link().is_some();
    let link = tool(&LINK, "Link", linked || c.link.is_some(), enabled)
        .shortcut("⌘K")
        .on_press(|s: &mut Mail| s.compose.toggle_link())
        .build(k)
        .checked(linked)
        .key("format-link");
    let separator = || el().w(1.0).h(16.0).fill(k.c.border_weak);
    let mode = SegmentedControl::new(
        &["Visual", "HTML"],
        Some(usize::from(c.mode == Mode::Source)),
    )
    .label("Edit as")
    .fit()
    .on_pick(|s: &mut Mail, i, _| {
        s.compose
            .set_mode(if i == 0 { Mode::Visual } else { Mode::Source });
    })
    .build(k)
    .key("edit-mode");
    row()
        .key("format-bar")
        .w_full()
        .gap(2.0)
        .center_y()
        .child(mark(&BOLD, "Bold", Some("⌘B"), Mark::Bold))
        .child(mark(&ITALIC, "Italic", Some("⌘I"), Mark::Italic))
        .child(mark(&UNDERLINE, "Underline", Some("⌘U"), Mark::Underline))
        .child(mark(&STRIKE, "Strikethrough", None, Mark::Strike))
        .child(mark(&CODE, "Code", None, Mark::Code))
        .child(link)
        .child(row().px(6.0).child(separator()))
        .child(block_button(&BULLETS, "Bulleted list", Block::Bullet(0)))
        .child(block_button(&NUMBERS, "Numbered list", Block::Numbered(0)))
        .child(block_button(&QUOTE, "Quote", Block::Quote))
        .child(block_button(&CODE_BLOCK, "Code block", Block::Code))
        .child(el().grow(1.0))
        .child(mode)
}

/// A formatting button's element key: `format-bulleted-list`.
fn tool_key(label: &str) -> String {
    format!("format-{}", label.to_ascii_lowercase().replace(' ', "-"))
}

/// A formatting button, filled while its formatting is on.
fn tool(icon: &'static Icon, label: &str, on: bool, enabled: bool) -> IconButton {
    IconButton::new(icon)
        .variant(if on {
            IconButtonVariant::Neutral
        } else {
            IconButtonVariant::GhostMuted
        })
        .label(label)
        .help(label)
        .disabled(!enabled)
}

/// Where a link's address is typed, under the formatting bar.
fn link_row(c: &ComposeForm, link: &TextEdit, k: &Kit) -> El {
    let mut row = row()
        .key("link-row")
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            col().grow(1.0).min_w(0.0).child(
                TextInput::new(link)
                    .label("Link address")
                    .placeholder("https://example.com or someone@example.com")
                    .width(None)
                    .on_edit(|s: &mut Mail| s.compose.link.get_or_insert_with(TextEdit::default))
                    .on_submit(|s: &mut Mail, _| s.compose.apply_link())
                    .build(k),
            ),
        )
        .child(
            Button::new("Link")
                .variant(ButtonVariant::Neutral)
                .on_press(|s: &mut Mail| s.compose.apply_link())
                .build(k),
        );
    if c.text.active_link().is_some() {
        row = row.child(
            Button::new("Remove link")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut Mail| s.compose.remove_link())
                .build(k),
        );
    }
    row.child(
        Button::new("Cancel")
            .variant(ButtonVariant::Ghost)
            .on_press(|s: &mut Mail| s.compose.toggle_link())
            .build(k),
    )
}

/// The message replied to, below the editor: its header, and the message itself when opened.
fn quote_view(c: &ComposeForm, k: &Kit) -> Option<El> {
    let quote = c.quote.as_ref()?;
    let head = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            text(quote.header.clone())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        )
        .child(
            Button::new(if c.expanded.quote {
                "Hide quote"
            } else {
                "Show quote"
            })
            .variant(ButtonVariant::Ghost)
            .expanded(c.expanded.quote)
            .on_press(|s: &mut Mail| s.compose.expanded.quote = !s.compose.expanded.quote)
            .build(k),
        )
        .child(
            Button::new("Remove quote")
                .variant(ButtonVariant::Ghost)
                .disabled(c.sending)
                .on_press(|s: &mut Mail| s.compose.remove_quote())
                .build(k),
        );
    let mut view = col().key("quote").w_full().gap(8.0).child(head);
    if c.expanded.quote {
        let body = c.quote_markdown.as_ref().map_or_else(
            || {
                text(quote.text.clone())
                    .font(k.ty.ui)
                    .color(k.c.text_muted)
                    .wrap()
                    .selectable(true)
            },
            |markdown| Markdown::new(markdown.clone()).selectable(true).build(k),
        );
        view = view.child(
            col()
                .key("quote-body")
                .w_full()
                .pl(12.0)
                .border_sides([0.0, 0.0, 0.0, 2.0], k.c.border_base)
                .child(body),
        );
    }
    Some(view)
}

/// How well the major mail apps support the message's markup, and, when opened, what each
/// doesn't.
fn compatibility(c: &ComposeForm, k: &Kit) -> El {
    let report = &c.compat;
    let percent = report.percent();
    let color = match percent {
        100 => k.c.success.fg,
        80..=99 => k.c.warning.fg,
        _ => k.c.danger.fg,
    };
    let unsupported = report
        .issues
        .iter()
        .filter(|i| !i.unsupported.is_empty())
        .count();
    let caveats = report.issues.len() - unsupported;
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let summary = match (unsupported, caveats) {
        (0, 0) => {
            "Shows as written in Gmail, Outlook, Apple Mail, Yahoo and Thunderbird".to_owned()
        }
        (0, n) => format!(
            "Shows in every major mail app, with {n} caveat{}",
            plural(n)
        ),
        (n, _) => format!("{n} feature{} won't show in some mail apps", plural(n)),
    };
    let mut head = row()
        .w_full()
        .gap(10.0)
        .center_y()
        .child(
            text("Compatibility")
                .font(k.ty.small_medium)
                .color(k.c.text_muted),
        )
        .child(
            text(format!("{percent}%"))
                .key("compat-percent")
                .font(k.ty.ui_medium)
                .color(color)
                .help(
                    "The share of mail apps (Gmail, Outlook, Apple Mail, Yahoo and Thunderbird, on \
                     each platform) that show everything this message uses, each provider counted \
                     equally. Apps that support something only partly still count.",
                ),
        )
        .child(
            text(summary)
                .font(k.ty.small)
                .color(k.c.text_muted)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        );
    if report.has_issues() || !report.providers.is_empty() {
        head = head.child(
            Button::new(if c.expanded.compat {
                "Hide details"
            } else {
                "Details"
            })
            .variant(ButtonVariant::Ghost)
            .expanded(c.expanded.compat)
            .on_press(|s: &mut Mail| s.compose.expanded.compat = !s.compose.expanded.compat)
            .build(k)
            .key("compat-details"),
        );
    }
    let mut panel = col()
        .key("compat")
        .w_full()
        .gap(10.0)
        .padding(12.0)
        .radius(8.0)
        .border(1.0, k.c.border_weak)
        .child(head);
    if c.expanded.compat {
        panel = compat_details(panel, report, k);
    }
    panel
}

/// The compatibility panel's details: each provider's score, then each feature not every app
/// fully supports.
fn compat_details(mut panel: El, report: &compat::Report, k: &Kit) -> El {
    let mut providers = row().gap(16.0).wrap();
    for (provider, score) in &report.providers {
        providers = providers.child(
            row()
                .gap(4.0)
                .child(text(provider.name()).font(k.ty.small).color(k.c.text_muted))
                .child(
                    text(format!("{}%", compat::percent(*score)))
                        .font(k.ty.small_medium)
                        .color(k.c.text_base),
                ),
        );
    }
    panel = panel.child(providers);
    for issue in &report.issues {
        let mut item = col().gap(2.0).child(
            text(issue.feature.title.to_owned())
                .font(k.ty.small_medium)
                .color(k.c.text_base),
        );
        if !issue.unsupported.is_empty() {
            item = item.child(
                text(format!(
                    "Not shown in {}",
                    clients_label(&issue.unsupported)
                ))
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap(),
            );
        }
        if !issue.partial.is_empty() {
            item = item.child(
                text(format!(
                    "Partly supported in {}",
                    clients_label(&issue.partial)
                ))
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
            );
        }
        panel = panel.child(item.key(format!("issue-{}", issue.feature.slug)));
    }
    panel.child(
        text(format!(
            "Support data from caniemail.com, updated {}.",
            compat::data_date()
        ))
        .font(k.ty.small)
        .color(k.c.text_faint),
    )
}

/// Clients grouped by provider: "Gmail (iOS, Android), Outlook (Windows)".
fn clients_label(clients: &[Client]) -> String {
    let all = compat::clients();
    Provider::ALL
        .iter()
        .filter_map(|provider| {
            let platforms: Vec<&str> = clients
                .iter()
                .filter(|c| c.provider == *provider)
                .map(|c| c.platform)
                .collect();
            if platforms.is_empty() {
                return None;
            }
            let everywhere = all.iter().filter(|c| c.provider == *provider).count();
            Some(if platforms.len() == everywhere {
                format!("{} (all)", provider.name())
            } else {
                format!("{} ({})", provider.name(), platforms.join(", "))
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}
