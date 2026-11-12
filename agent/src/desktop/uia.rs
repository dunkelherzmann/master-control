//! What is in a window on Windows, read through UI Automation.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, ExpandCollapseState_Collapsed, IUIAutomation, IUIAutomationElement, IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern,
    IUIAutomationLegacyIAccessiblePattern, IUIAutomationRangeValuePattern, IUIAutomationSelectionItemPattern, IUIAutomationTogglePattern,
    IUIAutomationTreeWalker, IUIAutomationValuePattern, ToggleState_On, UIA_ExpandCollapsePatternId, UIA_InvokePatternId,
    UIA_LegacyIAccessiblePatternId, UIA_RangeValuePatternId, UIA_SelectionItemPatternId, UIA_TogglePatternId, UIA_ValuePatternId,
};

use super::{common_role, DesktopError, Element, Rect, Result, TreeLimits};

struct Reading<'a> {
    walker: &'a IUIAutomationTreeWalker,
    limits: TreeLimits,
    read: usize,
    cut: bool,
}

/// UI Automation's control types by number (50000 is a button), in its own names.
fn control_type(id: i32) -> &'static str {
    const NAMES: [&str; 41] = [
        "button", "calendar", "check_box", "combo_box", "edit", "hyperlink", "image", "list_item", "list", "menu", "menu_bar", "menu_item",
        "progress_bar", "radio_button", "scroll_bar", "slider", "spinner", "status_bar", "tab_list", "tab_item", "label", "tool_bar",
        "tool_tip", "tree", "tree_item", "custom", "group", "thumb", "datagrid", "dataitem", "document", "split_button", "window", "pane",
        "header", "header_item", "table", "title_bar", "separator", "semantic_zoom", "app_bar",
    ];
    usize::try_from(id - 50000).ok().and_then(|index| NAMES.get(index)).copied().unwrap_or("custom")
}

impl Reading<'_> {
    /// SAFETY: must run on a thread where COM is initialised, with a live element.
    unsafe fn element(&mut self, element: &IUIAutomationElement, depth: u32) -> Element {
        self.read += 1;
        // Each property is asked for on its own and may fail on its own: the app can close a control under us.
        let frame = unsafe { element.CurrentBoundingRectangle() }.ok().filter(|frame| frame.right > frame.left && frame.bottom > frame.top).map(|frame| Rect {
            x: frame.left,
            y: frame.top,
            width: (frame.right - frame.left) as u32,
            height: (frame.bottom - frame.top) as u32,
        });
        let value = unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
            .ok()
            .and_then(|pattern| unsafe { pattern.CurrentValue() }.ok())
            .map(|value| value.to_string());
        let mut read = Element {
            role: common_role(control_type(unsafe { element.CurrentControlType() }.map_or(0, |kind| kind.0))),
            name: unsafe { element.CurrentName() }.map(|name| name.to_string()).unwrap_or_default(),
            value,
            frame,
            enabled: unsafe { element.CurrentIsEnabled() }.map_or(true, |enabled| enabled.as_bool()),
            focused: unsafe { element.CurrentHasKeyboardFocus() }.is_ok_and(|focused| focused.as_bool()),
            children: Vec::new(),
        };
        if depth >= self.limits.depth {
            return read;
        }
        let mut child = unsafe { self.walker.GetFirstChildElement(element) }.ok();
        while let Some(current) = child {
            if self.read >= self.limits.elements {
                self.cut = true;
                break;
            }
            read.children.push(unsafe { self.element(&current, depth + 1) });
            child = unsafe { self.walker.GetNextSiblingElement(&current) }.ok();
        }
        read
    }
}

pub fn tree(window: u64, limits: TreeLimits) -> Result<(Element, bool)> {
    let failed = |error: windows::core::Error| DesktopError::failed(format!("Windows: the window's contents couldn't be read: {error}"));
    // SAFETY: COM is initialised for this thread before any COM call and released after the last one, once every COM
    // object made here has been dropped.
    unsafe {
        let initialised = CoInitializeEx(None, COINIT_MULTITHREADED);
        let outcome = (|| {
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(failed)?;
            let root = automation.ElementFromHandle(HWND(window as usize as *mut core::ffi::c_void)).map_err(failed)?;
            let walker = automation.ControlViewWalker().map_err(failed)?;
            let mut reading = Reading { walker: &walker, limits, read: 0, cut: false };
            let element = reading.element(&root, 0);
            Ok((element, reading.cut))
        })();
        if initialised.is_ok() {
            CoUninitialize();
        }
        outcome
    }
}

/// Waits up to a second for something to become true.
fn settles(done: impl Fn() -> bool) {
    for _ in 0..40 {
        if done() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Does something to one element of a window, found again by the way the tree led to it, through the control
/// patterns UI Automation gives each kind of element.
pub fn act(window: u64, target: super::Target, action: &super::ElementAction) -> Result<super::Acted> {
    use super::{Acted, ElementAction};
    let failed = |error: windows::core::Error| DesktopError::failed(format!("Windows: the element didn't do it: {error}"));
    let cannot = |what: &str| DesktopError::not_supported(format!("a {} can't be {what}", target.role));
    // SAFETY: COM is initialised for this thread before any COM call and released after the last one, once every COM
    // object made here has been dropped.
    unsafe {
        let initialised = CoInitializeEx(None, COINIT_MULTITHREADED);
        let outcome = (|| {
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(failed)?;
            let mut at = automation.ElementFromHandle(HWND(window as usize as *mut core::ffi::c_void)).map_err(|_| DesktopError::gone("its window is gone"))?;
            let walker = automation.ControlViewWalker().map_err(failed)?;
            let gone = |_| DesktopError::gone("the window no longer has it");
            for &index in target.path {
                let mut child = walker.GetFirstChildElement(&at).map_err(gone)?;
                for _ in 0..index {
                    child = walker.GetNextSiblingElement(&child).map_err(gone)?;
                }
                at = child;
            }
            let role = common_role(control_type(at.CurrentControlType().map_or(0, |kind| kind.0)));
            target.check(&role, &at.CurrentName().map(|name| name.to_string()).unwrap_or_default())?;
            match action {
                ElementAction::Press => {
                    if let Ok(pattern) = at.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) {
                        pattern.Invoke().map_err(failed)?;
                    } else if let Ok(pattern) = at.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(UIA_LegacyIAccessiblePatternId) {
                        pattern.DoDefaultAction().map_err(|_| cannot("pressed"))?;
                    } else {
                        return Err(cannot("pressed"));
                    }
                    Ok(Acted::default())
                }
                ElementAction::SetValue(wanted) => {
                    if let Ok(pattern) = at.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                        pattern.SetValue(&windows::core::BSTR::from(wanted.as_str())).map_err(failed)?;
                        Ok(Acted { value: pattern.CurrentValue().ok().map(|value| value.to_string()), ..Acted::default() })
                    } else if let Ok(pattern) = at.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId) {
                        let number: f64 = wanted.trim().parse().map_err(|_| DesktopError { code: "bad_input", message: format!("it holds a number, and \"{wanted}\" isn't one") })?;
                        pattern.SetValue(number).map_err(failed)?;
                        Ok(Acted { value: pattern.CurrentValue().ok().map(|value| value.to_string()), ..Acted::default() })
                    } else {
                        Err(cannot("given a value"))
                    }
                }
                ElementAction::Toggle(wanted) => {
                    let pattern = at.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId).map_err(|_| cannot("ticked"))?;
                    let on = || pattern.CurrentToggleState().is_ok_and(|state| state == ToggleState_On);
                    let before = on();
                    if *wanted != Some(before) {
                        pattern.Toggle().map_err(failed)?;
                        // The control changes when its window next handles its messages: wait for that, briefly.
                        settles(|| on() != before);
                    }
                    Ok(Acted { on: Some(on()), ..Acted::default() })
                }
                ElementAction::Expand(wanted) => {
                    let pattern = at.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId).map_err(|_| cannot("opened or closed"))?;
                    if *wanted { pattern.Expand() } else { pattern.Collapse() }.map_err(failed)?;
                    let open = || pattern.CurrentExpandCollapseState().map_or(*wanted, |state| state != ExpandCollapseState_Collapsed);
                    settles(|| open() == *wanted);
                    let expanded = open();
                    Ok(Acted { expanded: Some(expanded), ..Acted::default() })
                }
                ElementAction::Focus => {
                    at.SetFocus().map_err(|_| cannot("given the keyboard"))?;
                    Ok(Acted::default())
                }
                ElementAction::Select => {
                    let pattern = at.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId).map_err(|_| cannot("selected"))?;
                    pattern.Select().map_err(failed)?;
                    Ok(Acted::default())
                }
            }
        })();
        if initialised.is_ok() {
            CoUninitialize();
        }
        outcome
    }
}

/// What has the keyboard on this desktop: its role and name, as the tree would tell them.
pub fn keyboard_owner() -> Option<(String, String)> {
    // SAFETY: COM is initialised for this thread before any COM call and released after the last one, once every COM
    // object made here has been dropped.
    unsafe {
        let initialised = CoInitializeEx(None, COINIT_MULTITHREADED);
        let owner = (|| {
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
            let focused = automation.GetFocusedElement().ok()?;
            let role = common_role(control_type(focused.CurrentControlType().map_or(0, |kind| kind.0)));
            Some((role, focused.CurrentName().map(|name| name.to_string()).unwrap_or_default()))
        })();
        if initialised.is_ok() {
            CoUninitialize();
        }
        owner
    }
}
