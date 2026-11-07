use std::rc::Rc;

use chrono::{Datelike as _, Days, Months, NaiveDate};
use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::calendar::{CalendarState, Date};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, RenderOnce, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use super::button::Button;
use crate::i18n::t;

type Selected = Rc<dyn Fn(Date, &mut Window, &mut App)>;

fn month_key(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => unreachable!("CalendarState supplies a valid month"),
    }
}

fn month_name(month: u32) -> SharedString {
    let key = month_key(month);
    let key = format!("gpui_component.Calendar.month.{key}");
    t!(&key).into_owned().into()
}

#[derive(Clone, Copy)]
struct DayFocus {
    date: NaiveDate,
    requested: bool,
}

#[derive(IntoElement)]
pub(in crate::gpui_app) struct Calendar {
    state: Entity<CalendarState>,
    selected: Option<Selected>,
    autofocus: bool,
    disabled: bool,
}
impl Calendar {
    pub(in crate::gpui_app) fn new(state: &Entity<CalendarState>) -> Self {
        Self {
            state: state.clone(),
            selected: None,
            autofocus: false,
            disabled: false,
        }
    }
    pub(in crate::gpui_app) fn on_selected(
        mut self,
        handler: impl Fn(Date, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.selected = Some(Rc::new(handler));
        self
    }
    pub(in crate::gpui_app) fn autofocus(mut self) -> Self {
        self.autofocus = true;
        self
    }
    pub(in crate::gpui_app) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

fn navigate_date(date: NaiveDate, key: &str, shift: bool) -> Option<NaiveDate> {
    match key {
        "left" => date.checked_sub_days(Days::new(1)),
        "right" => date.checked_add_days(Days::new(1)),
        "up" => date.checked_sub_days(Days::new(7)),
        "down" => date.checked_add_days(Days::new(7)),
        "home" => date.checked_sub_days(Days::new(date.weekday().num_days_from_sunday() as u64)),
        "end" => date.checked_add_days(Days::new(6 - date.weekday().num_days_from_sunday() as u64)),
        "pageup" => date.checked_sub_months(Months::new(if shift { 12 } else { 1 })),
        "pagedown" => date.checked_add_months(Months::new(if shift { 12 } else { 1 })),
        _ => None,
    }
}

impl RenderOnce for Calendar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id: ElementId = ("calendar-port", self.state.entity_id()).into();
        let state = self.state.read(cx);
        let (year, month, today, selection, weeks) = (
            state.current_year(),
            state.current_month() as u32,
            state.today(),
            state.date(),
            state.days(),
        );
        let initial = selection.start().unwrap_or(today);
        let focus = window.use_keyed_state((id.clone(), "day-focus"), cx, |_, _| DayFocus {
            date: initial,
            requested: self.autofocus,
        });
        let mut day_focus = *focus.read(cx);
        if !weeks.iter().flatten().any(|date| *date == day_focus.date) {
            day_focus.date = NaiveDate::from_ymd_opt(year, month, 1).expect("CalendarState month");
            focus.update(cx, |focus, _| *focus = day_focus);
        }
        let previous = self.state.clone();
        let next = self.state.clone();
        let month_state = self.state.clone();
        let year_state = self.state.clone();
        let header = h_flex()
            .h(px(28.))
            .w_full()
            .justify_between()
            .gap(px(4.))
            .child(
                Button::new("calendar-prev")
                    .disabled(self.disabled)
                    .ghost()
                    .small()
                    .size(px(28.))
                    .icon(IconName::ChevronLeft)
                    .accessibility_label(t!("common.calendar.previous_month").into_owned())
                    .on_click(move |_, _, cx| {
                        previous.update(cx, |state, cx| {
                            state.prev_month();
                            cx.notify();
                        })
                    }),
            )
            .child(
                Button::new("calendar-month")
                    .disabled(self.disabled)
                    .ghost()
                    .small()
                    .popup_trigger()
                    .px(px(2.))
                    .gap(px(4.))
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .accessibility_label(month_name(month))
                    .label({
                        let key = format!(
                            "common.calendar.short_month.{}",
                            month_key(month).to_ascii_lowercase()
                        );
                        t!(&key).into_owned()
                    })
                    .child(Icon::new(IconName::ChevronDown).size(px(14.)))
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu.scrollable(true);
                        for listed_month in 1..=12 {
                            let state = month_state.clone();
                            menu = menu.item(
                                PopupMenuItem::new(month_name(listed_month))
                                    .checked(listed_month == month)
                                    .on_click(move |_, _, cx| {
                                        state.update(cx, |state, cx| {
                                            state.select_month(listed_month as u8);
                                            cx.notify();
                                        })
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("calendar-year")
                    .disabled(self.disabled)
                    .ghost()
                    .small()
                    .popup_trigger()
                    .px(px(2.))
                    .gap(px(4.))
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .label(year.to_string())
                    .child(Icon::new(IconName::ChevronDown).size(px(14.)))
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu.scrollable(true);
                        // The app uses CalendarState's default ±50-year selector.
                        for listed_year in (today.year() - 50)..(today.year() + 50) {
                            let state = year_state.clone();
                            menu = menu.item(
                                PopupMenuItem::new(listed_year.to_string())
                                    .checked(listed_year == year)
                                    .on_click(move |_, _, cx| {
                                        state.update(cx, |state, cx| {
                                            state.select_year(listed_year);
                                            cx.notify();
                                        })
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("calendar-next")
                    .disabled(self.disabled)
                    .ghost()
                    .small()
                    .size(px(28.))
                    .icon(IconName::ChevronRight)
                    .accessibility_label(t!("common.calendar.next_month").into_owned())
                    .on_click(move |_, _, cx| {
                        next.update(cx, |state, cx| {
                            state.next_month();
                            cx.notify();
                        })
                    }),
            );

        let weekdays = h_flex().children((0..7).map(|day| {
            let key = format!("gpui_component.Calendar.week.{day}");
            div()
                .w(px(28.))
                .text_center()
                .text_size(px(12.8))
                .line_height(px(19.2))
                .text_color(cx.theme().muted_foreground)
                .child(t!(&key).into_owned())
        }));
        let mut rows = v_flex().gap(px(8.)).child(weekdays);
        let mut handles = Vec::new();
        for (week_index, week) in weeks.into_iter().enumerate() {
            let mut row = h_flex().id(("calendar-week", week_index)).role(Role::Row);
            for date in week {
                let handle = window
                    .use_keyed_state((id.clone(), date.to_string()), cx, |_, cx| {
                        cx.focus_handle()
                    })
                    .read(cx)
                    .clone();
                let disabled = self.disabled
                    || self
                        .state
                        .read(cx)
                        .disabled_matcher_ref()
                        .is_some_and(|matcher| matcher.matched(&date));
                if day_focus.requested && day_focus.date == date && !disabled {
                    handle.focus(window, cx);
                    focus.update(cx, |focus, _| focus.requested = false);
                }
                handles.push((date, handle.clone()));
                let endpoint = selection.is_active(&date);
                let middle = selection.is_in_range(&date) && !endpoint;
                let marked = endpoint || middle;
                let state = self.state.clone();
                let selected = self.selected.clone();
                let day_state = focus.clone();
                let fill = if marked {
                    cx.theme().button_primary
                } else if date == today {
                    cx.theme().accent
                } else {
                    cx.theme().transparent
                };
                let color = if marked {
                    cx.theme().button_primary_foreground
                } else if date.month() != month {
                    cx.theme().muted_foreground
                } else {
                    cx.theme().foreground
                };
                let hover = if marked {
                    cx.theme().button_primary
                } else {
                    cx.theme().muted
                };
                let bridge = selection.is_in_range(&date) && selection.start() != selection.end();
                let cell = div().relative().size(px(28.)).when(bridge, |cell| {
                    cell.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(if selection.start() == Some(date) {
                                px(14.)
                            } else {
                                px(0.)
                            })
                            .right(if selection.end() == Some(date) {
                                px(14.)
                            } else {
                                px(0.)
                            })
                            .bg(cx.theme().button_primary),
                    )
                });
                row = row.child(
                    cell.child(
                        Button::new(format!("calendar-{date}-0-{week_index}"))
                            .ghost()
                            .size(px(28.))
                            .p_0()
                            .border_0()
                            .rounded(if middle { px(0.) } else { px(8.) })
                            .font_normal()
                            .text_size(px(14.))
                            .line_height(px(14.))
                            .bg(fill)
                            .text_color(color)
                            .disabled(disabled)
                            .hover(move |style| style.bg(hover))
                            .tab_stop(day_focus.date == date)
                            .track_focus(&handle)
                            .accessibility_label(date.to_string())
                            .aria_selected(marked)
                            .label(date.day().to_string())
                            .on_click(move |_, window, cx| {
                                day_state.update(cx, |focus, _| {
                                    focus.date = date;
                                    focus.requested = false;
                                });
                                let complete =
                                    state.update(cx, |state, cx| state.activate_date(date, cx));
                                if complete && let Some(selected) = &selected {
                                    selected(state.read(cx).date(), window, cx);
                                }
                            }),
                    ),
                );
            }
            rows = rows.child(row);
        }
        let state = self.state.clone();
        v_flex()
            .id("calendar-grid")
            .test_support()
            .role(Role::Grid)
            .aria_label(format!("{} {year}", month_name(month)))
            .w(px(212.))
            .p(px(8.))
            .gap(px(16.))
            .bg(cx.theme().popover)
            .rounded(px(10.))
            .child(header)
            .child(rows)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if self.disabled {
                    return;
                }
                let Some((date, _)) = handles
                    .iter()
                    .find(|(_, handle): &&(NaiveDate, FocusHandle)| handle.is_focused(window))
                else {
                    return;
                };
                let Some(mut target) =
                    navigate_date(*date, &event.keystroke.key, event.keystroke.modifiers.shift)
                else {
                    return;
                };
                for _ in 0..366 {
                    if !state
                        .read(cx)
                        .disabled_matcher_ref()
                        .is_some_and(|matcher| matcher.matched(&target))
                    {
                        break;
                    }
                    let Some(next) = navigate_date(
                        target,
                        &event.keystroke.key,
                        event.keystroke.modifiers.shift,
                    ) else {
                        return;
                    };
                    if next == target {
                        return;
                    }
                    target = next;
                }
                if state
                    .read(cx)
                    .disabled_matcher_ref()
                    .is_some_and(|matcher| matcher.matched(&target))
                {
                    return;
                }
                state.update(cx, |state, cx| {
                    state.select_year(target.year());
                    state.select_month(target.month() as u8);
                    cx.notify();
                });
                focus.update(cx, |focus, cx| {
                    focus.date = target;
                    focus.requested = true;
                    cx.notify();
                });
                cx.stop_propagation();
            })
    }
}
