//! Receive the links macOS opens with Flectar Mail.
//!
//! macOS passes a URL to the application as a get-URL Apple Event, never on
//! the command line, also when the link is what launched it. AppKit turns the
//! event into `application:openURLs:`, which winit's application delegate
//! does not implement, so it is handled here.
//!
//! The handler has to be registered when AppKit finishes launching: AppKit
//! installs its own handlers first, replacing any registered earlier, and
//! delivers the event that launched the application right after. Apple's
//! scripting guide names `applicationWillFinishLaunching:` for this, and
//! later registration would lose the launch link (Zed hit exactly that).

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplicationWillFinishLaunchingNotification, NSRunningApplication};
use objc2_foundation::{NSAppleEventManager, NSNotificationCenter, NSObject, NSString};
use std::cell::{OnceCell, RefCell};

/// `kInternetEventClass`, `kAEGetURL` and `keyDirectObject`.
const INTERNET_EVENT_CLASS: u32 = u32::from_be_bytes(*b"GURL");
const GET_URL: u32 = u32::from_be_bytes(*b"GURL");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

/// Receives each URL on the main thread.
type UrlCallback = Box<dyn Fn(String)>;

thread_local! {
    /// Neither NSAppleEventManager nor NSNotificationCenter retains it.
    static HANDLER: OnceCell<Retained<UrlHandler>> = const { OnceCell::new() };
    static CALLBACK: RefCell<Option<UrlCallback>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FlectarMailUrlHandler"]
    struct UrlHandler;

    impl UrlHandler {
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &AnyObject) {
            register(self);
        }

        #[unsafe(method(handleGetURL:withReplyEvent:))]
        fn handle_get_url(&self, event: &AnyObject, _reply: &AnyObject) {
            // SAFETY: `event` is the NSAppleEventDescriptor of a get-URL
            // event; both selectors return a retained object or nil.
            // `paramDescriptorForKeyword:` takes an AEKeyword (UInt32).
            let url: Option<Retained<NSString>> = unsafe {
                let parameter: Option<Retained<AnyObject>> =
                    msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT];
                match parameter {
                    Some(parameter) => msg_send![&parameter, stringValue],
                    None => None,
                }
            };
            let Some(url) = url else {
                return;
            };
            CALLBACK.with(|callback| {
                if let Some(callback) = callback.borrow().as_ref() {
                    callback(url.to_string());
                }
            });
        }
    }
);

/// Call `callback` with each URL macOS opens with the application. Call it
/// before the event loop starts, so the link a launch was started for
/// arrives too.
pub(super) fn on_open_url(callback: impl Fn(String) + 'static) {
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    CALLBACK.with(|slot| *slot.borrow_mut() = Some(Box::new(callback)));
    HANDLER.with(|handler| {
        let handler = handler.get_or_init(|| {
            // SAFETY: `init` is NSObject's designated initializer.
            unsafe { msg_send![UrlHandler::alloc(main_thread), init] }
        });
        // Registering now would be overwritten by AppKit's own handler,
        // unless launching is already over.
        if NSRunningApplication::currentApplication().isFinishedLaunching() {
            register(handler);
            return;
        }
        // SAFETY: the handler implements the selector with the signature a
        // notification observer needs, and HANDLER keeps it alive for the
        // life of the thread.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                handler,
                sel!(applicationWillFinishLaunching:),
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
            );
        }
    });
}

fn register(handler: &UrlHandler) {
    let manager = NSAppleEventManager::sharedAppleEventManager();
    // SAFETY: the handler implements the selector with the signature
    // NSAppleEventManager calls, and HANDLER keeps it alive.
    unsafe {
        let _: () = msg_send![
            &manager,
            setEventHandler: handler,
            andSelector: sel!(handleGetURL:withReplyEvent:),
            forEventClass: INTERNET_EVENT_CLASS,
            andEventID: GET_URL,
        ];
    }
}
