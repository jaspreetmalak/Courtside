#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct MenuFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct ComposerMenuFrames {
    pub tools: MenuFrame,
    pub model: MenuFrame,
    pub visible: bool,
    pub tools_enabled: bool,
    pub model_enabled: bool,
}

pub fn composer_menu_owner() -> u64 {
    static NEXT_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT_OWNER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(not(target_os = "ios"))]
pub fn hide_keyboard_accessory() {}

#[cfg(not(target_os = "ios"))]
pub fn set_composer_menu_frames(_owner: u64, _frames: Option<ComposerMenuFrames>) {}

#[cfg(target_os = "ios")]
#[allow(unexpected_cfgs)]
mod ios {
    use super::{ComposerMenuFrames, MenuFrame};
    use block::ConcreteBlock;
    use objc::declare::ClassDecl;
    use objc::runtime::{object_getClass, Class, Object, Sel, BOOL, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};
    use objc::{Encode, Encoding};
    use std::cell::RefCell;
    use std::ffi::{c_void, CString};
    use std::ptr;
    use std::sync::OnceLock;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    unsafe impl Encode for CGRect {
        fn encode() -> Encoding {
            unsafe { Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
        }
    }

    struct ComposerMenus {
        owner: u64,
        closed: bool,
        tools: *mut Object,
        model: *mut Object,
    }

    thread_local! {
        // All access is dispatched to UIKit's main thread.
        static MENUS: RefCell<ComposerMenus> = const { RefCell::new(ComposerMenus {
            owner: 0,
            closed: false,
            tools: ptr::null_mut(),
            model: ptr::null_mut(),
        }) };
        static KEYBOARD_OBSERVERS: RefCell<Vec<*mut Object>> = const { RefCell::new(Vec::new()) };
    }

    extern "C" {
        static mut _dispatch_main_q: c_void;
        fn dispatch_async_f(
            queue: *mut c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
        fn object_setClass(object: *mut Object, class: *const Class) -> *const Class;
    }

    #[link(name = "WebKit", kind = "framework")]
    extern "C" {}

    #[link(name = "UIKit", kind = "framework")]
    extern "C" {
        static UIKeyboardWillChangeFrameNotification: *mut Object;
        static UIKeyboardDidChangeFrameNotification: *mut Object;
        static UIKeyboardDidHideNotification: *mut Object;
        static UIKeyboardFrameEndUserInfoKey: *mut Object;
        static UIKeyboardAnimationDurationUserInfoKey: *mut Object;
        static UIKeyboardAnimationCurveUserInfoKey: *mut Object;
        static UIKeyboardIsLocalUserInfoKey: *mut Object;
    }

    extern "C" fn empty_accessory_view(_content_view: &Object, _command: Sel) -> *mut Object {
        ptr::null_mut()
    }

    unsafe fn accessoryless_content_class(superclass: &'static Class) -> &'static Class {
        static CLASS: OnceLock<&'static Class> = OnceLock::new();
        CLASS.get_or_init(|| {
            let mut declaration = ClassDecl::new("CourtsideAccessorylessContentView", superclass)
                .expect("keyboard content subclass must only be registered once");
            declaration.add_method(
                sel!(inputAccessoryView),
                empty_accessory_view as extern "C" fn(&Object, Sel) -> *mut Object,
            );
            declaration.register()
        })
    }

    unsafe fn remove_accessory_from_tree(view: *mut Object) -> bool {
        if view.is_null() {
            return false;
        }

        let view_class = &*object_getClass(view);
        let class_name = view_class.name();
        if class_name == "CourtsideAccessorylessContentView" {
            return true;
        }
        if class_name.contains("WKContentView") {
            let replacement = accessoryless_content_class(view_class);
            object_setClass(view, replacement);
            let _: () = msg_send![view, reloadInputViews];
            return true;
        }

        let subviews: *mut Object = msg_send![view, subviews];
        let count: usize = msg_send![subviews, count];
        for index in 0..count {
            let child: *mut Object = msg_send![subviews, objectAtIndex: index];
            if remove_accessory_from_tree(child) {
                return true;
            }
        }
        false
    }

    unsafe fn find_webview(view: *mut Object) -> *mut Object {
        if view.is_null() {
            return ptr::null_mut();
        }
        // Wry/Dioxus may install a WKWebView subclass.
        let is_webview: BOOL = msg_send![view, isKindOfClass: class!(WKWebView)];
        if is_webview != NO {
            return view;
        }
        let subviews: *mut Object = msg_send![view, subviews];
        let count: usize = msg_send![subviews, count];
        for index in 0..count {
            let child: *mut Object = msg_send![subviews, objectAtIndex: index];
            let match_view = find_webview(child);
            if !match_view.is_null() {
                return match_view;
            }
        }
        ptr::null_mut()
    }

    unsafe fn current_webview() -> *mut Object {
        let application: *mut Object = msg_send![class!(UIApplication), sharedApplication];
        let scenes: *mut Object = msg_send![application, connectedScenes];
        let enumerator: *mut Object = msg_send![scenes, objectEnumerator];
        loop {
            let scene: *mut Object = msg_send![enumerator, nextObject];
            if scene.is_null() {
                return ptr::null_mut();
            }
            let windows: *mut Object = msg_send![scene, windows];
            let count: usize = msg_send![windows, count];
            for index in 0..count {
                let window: *mut Object = msg_send![windows, objectAtIndex: index];
                let root: *mut Object = msg_send![window, rootViewController];
                if !root.is_null() {
                    let view: *mut Object = msg_send![root, view];
                    let webview = find_webview(view);
                    if !webview.is_null() {
                        return webview;
                    }
                }
            }
        }
    }

    unsafe fn ns_string(value: &str) -> *mut Object {
        let value = CString::new(value).expect("native menu label cannot contain null bytes");
        msg_send![class!(NSString), stringWithUTF8String: value.as_ptr()]
    }

    unsafe fn report_keyboard_frame(notification: *mut Object, finished: bool) {
        let info: *mut Object = msg_send![notification, userInfo];
        let local: *mut Object = msg_send![info, objectForKey: UIKeyboardIsLocalUserInfoKey];
        if !local.is_null() {
            let is_local: BOOL = msg_send![local, boolValue];
            if is_local == NO {
                return;
            }
        }
        let end_value: *mut Object = msg_send![info, objectForKey: UIKeyboardFrameEndUserInfoKey];
        let webview = current_webview();
        if end_value.is_null() || webview.is_null() {
            return;
        }
        let window: *mut Object = msg_send![webview, window];
        if window.is_null() {
            return;
        }
        // UIKit reports screen coordinates. Convert through the window's
        // coordinate space so rotation and safe areas are handled by UIKit.
        let screen: *mut Object = msg_send![window, screen];
        let coordinate_space: *mut Object = msg_send![screen, coordinateSpace];
        let end_frame: CGRect = msg_send![end_value, CGRectValue];
        let window_frame: CGRect =
            msg_send![window, convertRect: end_frame fromCoordinateSpace: coordinate_space];
        let keyboard: CGRect = msg_send![webview, convertRect: window_frame fromView: window];
        let bounds: CGRect = msg_send![webview, bounds];
        let docked = keyboard.width >= bounds.width * 0.8
            && keyboard.x < bounds.x + bounds.width
            && keyboard.x + keyboard.width > bounds.x
            && keyboard.y + keyboard.height >= bounds.y + bounds.height - 1.0;
        let height = if docked {
            (keyboard.y - bounds.y).clamp(0.0, bounds.height)
        } else {
            bounds.height
        };
        let duration_value: *mut Object =
            msg_send![info, objectForKey: UIKeyboardAnimationDurationUserInfoKey];
        let curve_value: *mut Object =
            msg_send![info, objectForKey: UIKeyboardAnimationCurveUserInfoKey];
        let duration: f64 = msg_send![duration_value, doubleValue];
        let curve: isize = msg_send![curve_value, integerValue];
        let payload = serde_json::json!({
            "height": height,
            "fullHeight": bounds.height,
            "width": bounds.width,
            "duration": if finished { 0.0 } else { duration.clamp(0.0, 1.0) },
            "curve": curve,
        });
        let javascript = format!("window.courtsideLayout?.keyboardChanged({payload});");
        let _: () = msg_send![webview,
            evaluateJavaScript: ns_string(&javascript)
            completionHandler: ptr::null_mut::<c_void>()
        ];
    }

    unsafe fn observe_keyboard_notifications() {
        KEYBOARD_OBSERVERS.with(|state| {
            let mut observers = state.borrow_mut();
            if !observers.is_empty() {
                return;
            }
            let center: *mut Object = msg_send![class!(NSNotificationCenter), defaultCenter];
            let queue: *mut Object = msg_send![class!(NSOperationQueue), mainQueue];
            for (name, finished) in [
                (UIKeyboardWillChangeFrameNotification, false),
                (UIKeyboardDidChangeFrameNotification, true),
                (UIKeyboardDidHideNotification, true),
            ] {
                let handler = ConcreteBlock::new(move |notification: *mut Object| {
                    report_keyboard_frame(notification, finished);
                })
                .copy();
                let observer: *mut Object = msg_send![center,
                    addObserverForName: name
                    object: ptr::null_mut::<Object>()
                    queue: queue
                    usingBlock: &*handler
                ];
                let _: *mut Object = msg_send![observer, retain];
                observers.push(observer);
            }
        });
    }

    unsafe fn menu_action(
        webview: *mut Object,
        title: &str,
        subtitle: &str,
        symbol: &str,
        javascript: &'static str,
        selected: bool,
    ) -> *mut Object {
        let address = webview as usize;
        let handler = ConcreteBlock::new(move |_action: *mut Object| {
            let webview = address as *mut Object;
            let _: () = msg_send![webview,
                evaluateJavaScript: ns_string(javascript)
                completionHandler: ptr::null_mut::<c_void>()
            ];
        })
        .copy();
        let image: *mut Object = msg_send![class!(UIImage), systemImageNamed: ns_string(symbol)];
        let action: *mut Object = msg_send![class!(UIAction),
            actionWithTitle: ns_string(title)
            image: image
            identifier: ptr::null_mut::<Object>()
            handler: &*handler
        ];
        let _: () = msg_send![action, setSubtitle: ns_string(subtitle)];
        if selected {
            let _: () = msg_send![action, setState: 1isize];
        }
        action
    }

    unsafe fn make_menu_button(
        webview: *mut Object,
        host: *mut Object,
        model: bool,
    ) -> *mut Object {
        // UIKit owns the interaction and menu presentation. The visible plus
        // and model label stay in the DOM, avoiding duplicate labels, wrapping,
        // and Liquid Glass controls moving outside the composer on keyboard resize.
        let button: *mut Object = msg_send![class!(UIButton), buttonWithType: 0isize];
        let _: *mut Object = msg_send![button, retain];
        let action = if model {
            menu_action(
                webview,
                "GPT Live",
                "Realtime call model",
                "waveform",
                "document.getElementById('courtside-native-model-action')?.click();",
                true,
            )
        } else {
            menu_action(
                webview,
                "Choose Contact",
                "Select someone to call",
                "person.crop.circle.badge.plus",
                "document.getElementById('courtside-native-contact-action')?.click();",
                false,
            )
        };
        let children: *mut Object = msg_send![class!(NSMutableArray), arrayWithCapacity: 1usize];
        let _: () = msg_send![children, addObject: action];
        let menu: *mut Object = msg_send![class!(UIMenu),
            menuWithTitle: ns_string("")
            children: children
        ];
        let _: () = msg_send![button, setMenu: menu];
        let _: () = msg_send![button, setShowsMenuAsPrimaryAction: YES];
        // Keep UIButton from drawing a selected title over the DOM label.
        let _: () = msg_send![button, setChangesSelectionAsPrimaryAction: NO];
        let label = if model {
            "Choose call model"
        } else {
            "Add to message"
        };
        let identifier = if model {
            "courtside-model-menu"
        } else {
            "courtside-contact-menu"
        };
        let _: () = msg_send![button, setAccessibilityLabel: ns_string(label)];
        let _: () = msg_send![button, setAccessibilityIdentifier: ns_string(identifier)];
        if model {
            let _: () = msg_send![button, setAccessibilityValue: ns_string("GPT Live")];
        }
        // Keep the hit targets above WKWebView, so its editing/scrolling
        // gesture recognizers do not intercept menu touches with the keyboard up.
        let _: () = msg_send![host, addSubview: button];
        button
    }

    unsafe fn remove_menu_buttons(menus: &mut ComposerMenus) {
        for button in [&mut menus.tools, &mut menus.model] {
            if !(*button).is_null() {
                let _: () = msg_send![*button, removeFromSuperview];
                let _: () = msg_send![*button, release];
                *button = ptr::null_mut();
            }
        }
    }

    unsafe fn place_menu_button(
        button: *mut Object,
        webview: *mut Object,
        host: *mut Object,
        frame: MenuFrame,
        visible: bool,
        enabled: bool,
    ) {
        let bounds: CGRect = msg_send![webview, bounds];
        let webview_frame = CGRect {
            x: bounds.x + frame.x,
            y: bounds.y + frame.y,
            width: frame.width,
            height: frame.height,
        };
        let frame: CGRect = msg_send![webview, convertRect: webview_frame toView: host];
        let parent: *mut Object = msg_send![button, superview];
        if parent != host {
            let _: () = msg_send![host, addSubview: button];
        }
        let _: () = msg_send![button, setFrame: frame];
        let _: () = msg_send![button, setHidden: if visible { NO } else { YES }];
        let _: () = msg_send![button, setEnabled: if enabled { YES } else { NO }];
        let _: () = msg_send![host, bringSubviewToFront: button];
    }

    extern "C" fn update_menu_frames_on_main(context: *mut c_void) {
        let (owner, frames) =
            unsafe { *Box::from_raw(context as *mut (u64, Option<ComposerMenuFrames>)) };
        MENUS.with(|state| unsafe {
            let mut menus = state.borrow_mut();
            // An old screen's delayed observer/drop must not replace the new
            // screen's menus, or resurrect controls after it has unmounted.
            if owner < menus.owner || (owner == menus.owner && menus.closed) {
                return;
            }
            if owner > menus.owner {
                remove_menu_buttons(&mut menus);
                menus.owner = owner;
                menus.closed = false;
            }
            let Some(frames) = frames else {
                remove_menu_buttons(&mut menus);
                menus.closed = true;
                return;
            };
            let webview = current_webview();
            if webview.is_null() {
                return;
            }
            let host: *mut Object = msg_send![webview, superview];
            if host.is_null() {
                return;
            }
            if menus.tools.is_null() {
                menus.tools = make_menu_button(webview, host, false);
                menus.model = make_menu_button(webview, host, true);
            }
            place_menu_button(
                menus.tools,
                webview,
                host,
                frames.tools,
                frames.visible,
                frames.tools_enabled,
            );
            place_menu_button(
                menus.model,
                webview,
                host,
                frames.model,
                frames.visible,
                frames.model_enabled,
            );
        });
    }

    extern "C" fn patch_webview_on_main(_context: *mut c_void) {
        unsafe {
            observe_keyboard_notifications();
            let application: *mut Object = msg_send![class!(UIApplication), sharedApplication];
            let scenes: *mut Object = msg_send![application, connectedScenes];
            let enumerator: *mut Object = msg_send![scenes, objectEnumerator];
            loop {
                let scene: *mut Object = msg_send![enumerator, nextObject];
                if scene.is_null() {
                    break;
                }
                let windows: *mut Object = msg_send![scene, windows];
                let count: usize = msg_send![windows, count];
                for index in 0..count {
                    let window: *mut Object = msg_send![windows, objectAtIndex: index];
                    let root: *mut Object = msg_send![window, rootViewController];
                    if root.is_null() {
                        continue;
                    }
                    let view: *mut Object = msg_send![root, view];
                    let webview = find_webview(view);
                    if !webview.is_null() {
                        // The DOM owns scrolling inside the transcript. Keep
                        // UIKit from rubber-banding the entire app on focus.
                        let scroll_view: *mut Object = msg_send![webview, scrollView];
                        let _: () = msg_send![scroll_view, setBounces: NO];
                        let _: () = msg_send![scroll_view, setAlwaysBounceVertical: NO];
                        let _: () = msg_send![scroll_view, setAlwaysBounceHorizontal: NO];
                    }
                    if remove_accessory_from_tree(view) {
                        return;
                    }
                }
            }
        }
    }

    pub fn hide_keyboard_accessory() {
        unsafe {
            dispatch_async_f(
                std::ptr::addr_of_mut!(_dispatch_main_q),
                ptr::null_mut(),
                patch_webview_on_main,
            );
        }
    }

    extern "C" fn copy_text_on_main(context: *mut c_void) {
        let value = unsafe { Box::from_raw(context as *mut String) };
        unsafe {
            let pasteboard: *mut Object = msg_send![class!(UIPasteboard), generalPasteboard];
            let _: () = msg_send![pasteboard, setString: ns_string(&value)];
        }
    }

    pub fn copy_text(value: String) {
        let context = Box::into_raw(Box::new(value)) as *mut c_void;
        unsafe {
            dispatch_async_f(
                std::ptr::addr_of_mut!(_dispatch_main_q),
                context,
                copy_text_on_main,
            );
        }
    }

    pub fn set_composer_menu_frames(owner: u64, frames: Option<ComposerMenuFrames>) {
        let context = Box::into_raw(Box::new((owner, frames))) as *mut c_void;
        unsafe {
            dispatch_async_f(
                std::ptr::addr_of_mut!(_dispatch_main_q),
                context,
                update_menu_frames_on_main,
            );
        }
    }
}

#[cfg(target_os = "ios")]
pub use ios::{copy_text, hide_keyboard_accessory, set_composer_menu_frames};
