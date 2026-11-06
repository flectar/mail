#import <UIKit/UIKit.h>
#include <stdbool.h>

extern void flectar_memory_pressure(bool critical);

// Forward UIKit's memory signals to Rust, which frees caches it can rebuild
// and returns free pages. A memory warning is critical; entering the
// background asks for the moderate release. NotificationCenter retains these
// process-lifetime observer blocks; install once on the main queue.
void install_memory_pressure_observers(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        static dispatch_once_t once;
        dispatch_once(&once, ^{
            NSNotificationCenter *center = NSNotificationCenter.defaultCenter;
            [center addObserverForName:UIApplicationDidReceiveMemoryWarningNotification
                                object:nil
                                 queue:NSOperationQueue.mainQueue
                            usingBlock:^(NSNotification *notification) {
                (void)notification;
                flectar_memory_pressure(true);
            }];
            [center addObserverForName:UIApplicationDidEnterBackgroundNotification
                                object:nil
                                 queue:NSOperationQueue.mainQueue
                            usingBlock:^(NSNotification *notification) {
                (void)notification;
                flectar_memory_pressure(false);
            }];
        });
    });
}
