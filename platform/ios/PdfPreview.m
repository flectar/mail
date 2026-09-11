#import <UIKit/UIKit.h>

extern void flectar_suspend_pdf_preview(void);

// NotificationCenter retains these process-lifetime observer blocks. Install
// once on the main queue; the Rust callback schedules UI work on Slint.
void install_pdf_lifecycle_observers(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        static dispatch_once_t once;
        dispatch_once(&once, ^{
            NSNotificationCenter *center = NSNotificationCenter.defaultCenter;
            for (NSNotificationName name in @[UIApplicationDidEnterBackgroundNotification,
                                              UIApplicationDidReceiveMemoryWarningNotification]) {
                [center addObserverForName:name object:nil queue:NSOperationQueue.mainQueue
                                usingBlock:^(NSNotification *notification) {
                    (void)notification;
                    flectar_suspend_pdf_preview();
                }];
            }
        });
    });
}
