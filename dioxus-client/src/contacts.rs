#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactSelection {
    pub name: String,
    pub phone_number: String,
}

#[cfg(not(target_os = "ios"))]
pub async fn pick_contact() -> Result<Option<ContactSelection>, String> {
    Err("Contact selection is available in the iPhone app.".into())
}

#[cfg(not(target_os = "ios"))]
pub async fn search_contacts(_query: String) -> Result<Vec<ContactSelection>, String> {
    Err("Contact search is available in the iPhone app.".into())
}

#[cfg(target_os = "ios")]
#[allow(unexpected_cfgs)]
mod ios {
    use super::ContactSelection;
    use block::ConcreteBlock;
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Protocol, Sel, BOOL, NO};
    use objc::{class, msg_send, sel, sel_impl};
    use std::collections::HashSet;
    use std::ffi::{c_char, c_void, CStr};
    use std::ptr;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    #[link(name = "ContactsUI", kind = "framework")]
    extern "C" {}

    #[link(name = "Contacts", kind = "framework")]
    extern "C" {
        static CNContactGivenNameKey: *mut Object;
        static CNContactFamilyNameKey: *mut Object;
        static CNContactOrganizationNameKey: *mut Object;
        static CNContactPhoneNumbersKey: *mut Object;
    }

    #[link(name = "UIKit", kind = "framework")]
    extern "C" {}

    extern "C" {
        // `dispatch_get_main_queue()` is an inline C wrapper around this
        // exported global queue object, so there is no function symbol to
        // link from Rust.
        static mut _dispatch_main_q: c_void;
        fn dispatch_async_f(
            queue: *mut c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
    }

    #[derive(Default)]
    struct PickerState {
        sender: Option<async_channel::Sender<Result<Option<ContactSelection>, String>>>,
        // Objective-C objects are kept as integer addresses so the shared
        // state remains Send. UIKit access itself is dispatched to main.
        delegate: Option<usize>,
    }

    fn picker_state() -> &'static Mutex<PickerState> {
        static STATE: OnceLock<Mutex<PickerState>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(PickerState::default()))
    }

    fn finish(result: Result<Option<ContactSelection>, String>) {
        let sender = picker_state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sender
            .take();
        if let Some(sender) = sender {
            let _ = sender.try_send(result);
        }
    }

    unsafe fn string_from_ns(value: *mut Object) -> String {
        if value.is_null() {
            return String::new();
        }
        let utf8: *const c_char = msg_send![value, UTF8String];
        if utf8.is_null() {
            return String::new();
        }
        CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }

    unsafe fn contact_name(contact: *mut Object) -> String {
        if contact.is_null() {
            return String::new();
        }
        let given: *mut Object = msg_send![contact, givenName];
        let family: *mut Object = msg_send![contact, familyName];
        let organization: *mut Object = msg_send![contact, organizationName];
        let personal = [string_from_ns(given), string_from_ns(family)]
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if personal.is_empty() {
            string_from_ns(organization).trim().to_string()
        } else {
            personal
        }
    }

    extern "C" fn picker_did_cancel(_delegate: &Object, _command: Sel, _picker: *mut Object) {
        finish(Ok(None));
    }

    extern "C" fn picker_did_select_property(
        _delegate: &Object,
        _command: Sel,
        _picker: *mut Object,
        property: *mut Object,
    ) {
        unsafe {
            if property.is_null() {
                finish(Err(
                    "The selected contact did not include a phone number.".into()
                ));
                return;
            }
            let contact: *mut Object = msg_send![property, contact];
            let phone: *mut Object = msg_send![property, value];
            let phone_value: *mut Object = if phone.is_null() {
                ptr::null_mut()
            } else {
                msg_send![phone, stringValue]
            };
            let phone_number = string_from_ns(phone_value).trim().to_string();
            if phone_number.is_empty() {
                finish(Err(
                    "The selected contact did not include a phone number.".into()
                ));
                return;
            }
            finish(Ok(Some(ContactSelection {
                name: contact_name(contact),
                phone_number,
            })));
        }
    }

    fn delegate_class() -> &'static Class {
        static DELEGATE_CLASS: OnceLock<&'static Class> = OnceLock::new();
        DELEGATE_CLASS.get_or_init(|| unsafe {
            let mut declaration =
                ClassDecl::new("CourtsideContactPickerDelegate", class!(NSObject))
                    .expect("contact picker delegate must only be registered once");
            if let Some(protocol) = Protocol::get("CNContactPickerDelegate") {
                declaration.add_protocol(protocol);
            }
            declaration.add_method(
                sel!(contactPickerDidCancel:),
                picker_did_cancel as extern "C" fn(&Object, Sel, *mut Object),
            );
            declaration.add_method(
                sel!(contactPicker:didSelectContactProperty:),
                picker_did_select_property as extern "C" fn(&Object, Sel, *mut Object, *mut Object),
            );
            declaration.register()
        })
    }

    unsafe fn top_view_controller() -> *mut Object {
        let application: *mut Object = msg_send![class!(UIApplication), sharedApplication];
        let scenes: *mut Object = msg_send![application, connectedScenes];
        let enumerator: *mut Object = msg_send![scenes, objectEnumerator];
        let mut fallback_window: *mut Object = ptr::null_mut();
        let mut key_window: *mut Object = ptr::null_mut();

        loop {
            let scene: *mut Object = msg_send![enumerator, nextObject];
            if scene.is_null() {
                break;
            }
            let windows: *mut Object = msg_send![scene, windows];
            let count: usize = msg_send![windows, count];
            for index in 0..count {
                let window: *mut Object = msg_send![windows, objectAtIndex: index];
                if fallback_window.is_null() {
                    fallback_window = window;
                }
                let is_key: BOOL = msg_send![window, isKeyWindow];
                if is_key != NO {
                    key_window = window;
                    break;
                }
            }
            if !key_window.is_null() {
                break;
            }
        }

        let window = if key_window.is_null() {
            fallback_window
        } else {
            key_window
        };
        if window.is_null() {
            return ptr::null_mut();
        }

        let mut controller: *mut Object = msg_send![window, rootViewController];
        while !controller.is_null() {
            let presented: *mut Object = msg_send![controller, presentedViewController];
            if presented.is_null() {
                break;
            }
            controller = presented;
        }
        controller
    }

    unsafe fn predicate(format: &str) -> *mut Object {
        let bytes = std::ffi::CString::new(format).expect("predicate has no null bytes");
        let format_string: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: bytes.as_ptr()];
        let arguments: *mut Object = msg_send![class!(NSArray), array];
        msg_send![class!(NSPredicate), predicateWithFormat: format_string argumentArray: arguments]
    }

    extern "C" fn present_picker_on_main(_context: *mut c_void) {
        unsafe {
            let presenter = top_view_controller();
            if presenter.is_null() {
                finish(Err("Courtside couldn't open Contacts right now.".into()));
                return;
            }

            let picker: *mut Object = msg_send![class!(CNContactPickerViewController), new];
            let delegate: *mut Object = msg_send![delegate_class(), new];

            let old_delegate = {
                let mut state = picker_state()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.delegate.replace(delegate as usize)
            };
            if let Some(old_delegate) = old_delegate {
                let _: () = msg_send![old_delegate as *mut Object, release];
            }

            let _: () = msg_send![picker, setDelegate: delegate];
            let property_keys: *mut Object =
                msg_send![class!(NSArray), arrayWithObject: CNContactPhoneNumbersKey];
            let _: () = msg_send![picker, setDisplayedPropertyKeys: property_keys];

            // Tapping a person drills into their phone numbers; tapping a
            // number returns that exact property instead of starting a call.
            let false_predicate: *mut Object =
                msg_send![class!(NSPredicate), predicateWithValue: NO];
            let _: () = msg_send![picker, setPredicateForSelectionOfContact: false_predicate];
            let enabled = predicate("phoneNumbers.@count > 0");
            let _: () = msg_send![picker, setPredicateForEnablingContact: enabled];
            let selectable = predicate("key == 'phoneNumbers'");
            let _: () = msg_send![picker, setPredicateForSelectionOfProperty: selectable];

            let _: () = msg_send![presenter,
                presentViewController: picker
                animated: true
                completion: ptr::null_mut::<c_void>()
            ];
            let _: () = msg_send![picker, release];
        }
    }

    pub async fn pick_contact() -> Result<Option<ContactSelection>, String> {
        let (sender, receiver) = async_channel::bounded(1);
        {
            let mut state = picker_state()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.sender.is_some() {
                return Err("The contact picker is already open.".into());
            }
            state.sender = Some(sender);
        }

        unsafe {
            let on_main: BOOL = msg_send![class!(NSThread), isMainThread];
            if on_main != NO {
                present_picker_on_main(ptr::null_mut());
            } else {
                dispatch_async_f(
                    std::ptr::addr_of_mut!(_dispatch_main_q),
                    ptr::null_mut(),
                    present_picker_on_main,
                );
            }
        }

        receiver
            .recv()
            .await
            .map_err(|_| "The contact picker closed unexpectedly.".to_string())?
    }

    unsafe fn search_contacts_sync(query: &str) -> Result<Vec<ContactSelection>, String> {
        let authorization: isize =
            msg_send![class!(CNContactStore), authorizationStatusForEntityType: 0isize];
        if authorization == 1 {
            return Err("Contacts access is restricted on this iPhone.".into());
        }
        if authorization == 2 {
            return Err("Contacts access is off. Enable Courtside in Settings → Privacy & Security → Contacts.".into());
        }

        let store: *mut Object = msg_send![class!(CNContactStore), new];
        if authorization == 0 {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            let completion = ConcreteBlock::new(move |granted: BOOL, error: *mut Object| {
                let result = if granted != NO {
                    Ok(())
                } else if error.is_null() {
                    Err(
                        "Contacts access wasn't allowed, so Courtside can't search names."
                            .to_string(),
                    )
                } else {
                    let description = unsafe {
                        let description: *mut Object = msg_send![error, localizedDescription];
                        string_from_ns(description)
                    };
                    if description.trim().is_empty() {
                        Err(
                            "Contacts access wasn't allowed, so Courtside can't search names."
                                .to_string(),
                        )
                    } else {
                        Err(description)
                    }
                };
                let _ = sender.send(result);
            })
            .copy();
            let _: () = msg_send![store,
                requestAccessForEntityType: 0isize
                completionHandler: &*completion
            ];
            let access = receiver
                .recv_timeout(Duration::from_secs(90))
                .map_err(|_| "Contacts permission timed out. Try @name again.".to_string())?;
            if let Err(error) = access {
                let _: () = msg_send![store, release];
                return Err(error);
            }
        }

        let query_bytes = std::ffi::CString::new(query).map_err(|_| "Invalid contact name.")?;
        let query_string: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: query_bytes.as_ptr()];
        let predicate: *mut Object =
            msg_send![class!(CNContact), predicateForContactsMatchingName: query_string];

        let keys: *mut Object = msg_send![class!(NSMutableArray), arrayWithCapacity: 4usize];
        let _: () = msg_send![keys, addObject: CNContactGivenNameKey];
        let _: () = msg_send![keys, addObject: CNContactFamilyNameKey];
        let _: () = msg_send![keys, addObject: CNContactOrganizationNameKey];
        let _: () = msg_send![keys, addObject: CNContactPhoneNumbersKey];

        let mut error: *mut Object = ptr::null_mut();
        let contacts: *mut Object = msg_send![store,
            unifiedContactsMatchingPredicate: predicate
            keysToFetch: keys
            error: &mut error
        ];
        if contacts.is_null() {
            let message = if error.is_null() {
                "Courtside couldn't search Contacts.".to_string()
            } else {
                let description: *mut Object = msg_send![error, localizedDescription];
                let description = string_from_ns(description);
                if description.trim().is_empty() {
                    "Courtside couldn't search Contacts.".to_string()
                } else {
                    description
                }
            };
            let _: () = msg_send![store, release];
            return Err(message);
        }

        let mut matches = Vec::new();
        let mut seen = HashSet::new();
        let contact_count: usize = msg_send![contacts, count];
        'contacts: for contact_index in 0..contact_count {
            let contact: *mut Object = msg_send![contacts, objectAtIndex: contact_index];
            let name = contact_name(contact);
            let phone_numbers: *mut Object = msg_send![contact, phoneNumbers];
            let phone_count: usize = msg_send![phone_numbers, count];
            for phone_index in 0..phone_count {
                let labeled: *mut Object = msg_send![phone_numbers, objectAtIndex: phone_index];
                let phone: *mut Object = msg_send![labeled, value];
                let phone_value: *mut Object = msg_send![phone, stringValue];
                let phone_number = string_from_ns(phone_value).trim().to_string();
                if phone_number.is_empty() {
                    continue;
                }
                let identity = format!("{}|{}", name.to_ascii_lowercase(), phone_number);
                if seen.insert(identity) {
                    matches.push(ContactSelection {
                        name: name.clone(),
                        phone_number,
                    });
                }
                if matches.len() >= 8 {
                    break 'contacts;
                }
            }
        }
        let _: () = msg_send![store, release];
        Ok(matches)
    }

    pub async fn search_contacts(query: String) -> Result<Vec<ContactSelection>, String> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || unsafe {
            let pool = objc::runtime::objc_autoreleasePoolPush();
            // Contact access is requested explicitly above. This worker keeps
            // both the permission wait and ContactStore fetch off the UI thread.
            let result = search_contacts_sync(query.trim());
            let _ = sender.send_blocking(result);
            objc::runtime::objc_autoreleasePoolPop(pool);
        });
        receiver
            .recv()
            .await
            .map_err(|_| "Contact search stopped unexpectedly.".to_string())?
    }
}

#[cfg(target_os = "ios")]
pub use ios::{pick_contact, search_contacts};
