// Reduce Motion for the app's own animations (src/motion.rs).

#import <UIKit/UIKit.h>

extern void flectar_reduce_motion_changed(void);

bool flectar_reduce_motion(void) {
    return UIAccessibilityIsReduceMotionEnabled();
}

void flectar_watch_reduce_motion(void) {
    [NSNotificationCenter.defaultCenter
        addObserverForName:UIAccessibilityReduceMotionStatusDidChangeNotification
                    object:nil
                     queue:NSOperationQueue.mainQueue
                usingBlock:^(NSNotification *notification) {
                    (void)notification;
                    flectar_reduce_motion_changed();
                }];
}
