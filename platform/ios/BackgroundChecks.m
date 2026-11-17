#import <BackgroundTasks/BackgroundTasks.h>
#import <UIKit/UIKit.h>
#include <os/lock.h>
#include <stdbool.h>

// Checking for new mail while Flectar Mail is in the background. Without a
// push server, iOS wakes the app now and then with a background app refresh
// task, at times it picks from how the app is used, and gives it about half a
// minute. src/platform/ios.rs owns the Rust side.

// Info.plist permits this identifier (BGTaskSchedulerPermittedIdentifiers).
static NSString *const FlectarCheckIdentifier = @"com.flectar.mail.check";

// The earliest a check may start after the user left the app or the last
// check ended. iOS decides when it really runs.
static const NSTimeInterval FlectarCheckInterval = 15 * 60;

void *flectar_stay_awake_begin(void);
void flectar_stay_awake_end(void *awake);

extern void *flectar_check_for_mail(void *context);
extern void flectar_cancel_check(void *check);
extern void flectar_release_check(void *check);
extern bool flectar_wants_background_checks(void);
extern void flectar_app_became_active(void);
extern void flectar_background_refresh_changed(void);

// Asks iOS for the next check, or cancels it when the user wants nothing a
// check brings. It may block, so it runs off the main thread.
static void flectar_schedule_check(void) {
    BGTaskScheduler *scheduler = BGTaskScheduler.sharedScheduler;
    if (!flectar_wants_background_checks()) {
        [scheduler cancelTaskRequestWithIdentifier:FlectarCheckIdentifier];
        return;
    }
    BGAppRefreshTaskRequest *request =
        [[BGAppRefreshTaskRequest alloc] initWithIdentifier:FlectarCheckIdentifier];
    request.earliestBeginDate = [NSDate dateWithTimeIntervalSinceNow:FlectarCheckInterval];
    NSError *error = nil;
    // Fails while Background App Refresh is off, which Settings explains.
    if (![scheduler submitTaskRequest:request error:&error]) {
        NSLog(@"could not schedule a check for new mail: %@", error);
    }
}

// Keeps the app running in the background, up to the half minute or so iOS
// allows, until it is ended or iOS wants the time back.
@interface FlectarAwake : NSObject
- (void)end;
@end

@implementation FlectarAwake {
    os_unfair_lock _lock;
    UIBackgroundTaskIdentifier _task;
}

- (instancetype)init {
    if ((self = [super init])) {
        _lock = OS_UNFAIR_LOCK_INIT;
        __weak FlectarAwake *weakSelf = self;
        // UIKit allows this from any thread.
        _task = [UIApplication.sharedApplication
            beginBackgroundTaskWithName:@"Flectar Mail"
                      expirationHandler:^{
                          [weakSelf end];
                      }];
    }
    return self;
}

- (void)end {
    os_unfair_lock_lock(&_lock);
    UIBackgroundTaskIdentifier task = _task;
    _task = UIBackgroundTaskInvalid;
    os_unfair_lock_unlock(&_lock);
    if (task != UIBackgroundTaskInvalid) {
        [UIApplication.sharedApplication endBackgroundTask:task];
    }
}

- (void)dealloc {
    [self end];
}
@end

void *flectar_stay_awake_begin(void) {
    return (__bridge_retained void *)[[FlectarAwake alloc] init];
}

void flectar_stay_awake_end(void *awake) {
    FlectarAwake *held = (__bridge_transfer FlectarAwake *)awake;
    [held end];
}

// One check iOS started. It is completed once: when the app has told of the
// new mail, or when iOS wants its time back, whichever comes first.
@interface FlectarCheck : NSObject
- (instancetype)initWithTask:(BGTask *)task;
- (void)start;
- (void)finish:(BOOL)success;
- (void)expire;
@end

@implementation FlectarCheck {
    BGTask *_task;
    os_unfair_lock _lock;
    // The Rust side's handle, released with this object.
    void *_check;
    BOOL _finished;
}

- (instancetype)initWithTask:(BGTask *)task {
    if ((self = [super init])) {
        _task = task;
        _lock = OS_UNFAIR_LOCK_INIT;
    }
    return self;
}

- (void)start {
    // The Rust side keeps a reference until it reports back through
    // flectar_check_finished.
    void *check = flectar_check_for_mail((__bridge_retained void *)self);
    os_unfair_lock_lock(&_lock);
    _check = check;
    os_unfair_lock_unlock(&_lock);
}

- (void)finish:(BOOL)success {
    os_unfair_lock_lock(&_lock);
    BOOL first = !_finished;
    _finished = YES;
    os_unfair_lock_unlock(&_lock);
    if (first) {
        // The next check, now that this one has settled what the app wants.
        flectar_schedule_check();
        [_task setTaskCompletedWithSuccess:success];
    }
}

- (void)expire {
    os_unfair_lock_lock(&_lock);
    void *check = _check;
    os_unfair_lock_unlock(&_lock);
    if (check) {
        flectar_cancel_check(check);
    }
    [self finish:NO];
}

- (void)dealloc {
    if (_check) {
        flectar_release_check(_check);
    }
}
@end

// The Rust side is done with a check; `context` is the reference it kept.
void flectar_check_finished(void *context, bool checked) {
    FlectarCheck *check = (__bridge_transfer FlectarCheck *)context;
    [check finish:checked];
}

// Called on the main thread before the app finishes launching, as iOS
// requires of a background task's launch handler.
void flectar_register_background_checks(void) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        [BGTaskScheduler.sharedScheduler
            registerForTaskWithIdentifier:FlectarCheckIdentifier
                               usingQueue:nil
                            launchHandler:^(BGTask *task) {
                                FlectarCheck *check = [[FlectarCheck alloc] initWithTask:task];
                                task.expirationHandler = ^{
                                    [check expire];
                                };
                                [check start];
                            }];
        NSNotificationCenter *center = NSNotificationCenter.defaultCenter;
        NSOperationQueue *main = NSOperationQueue.mainQueue;
        [center addObserverForName:UIApplicationDidEnterBackgroundNotification
                            object:nil
                             queue:main
                        usingBlock:^(NSNotification *notification) {
                            (void)notification;
                            void *awake = flectar_stay_awake_begin();
                            dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
                                flectar_schedule_check();
                                flectar_stay_awake_end(awake);
                            });
                        }];
        [center addObserverForName:UIApplicationDidBecomeActiveNotification
                            object:nil
                             queue:main
                        usingBlock:^(NSNotification *notification) {
                            (void)notification;
                            flectar_app_became_active();
                        }];
        for (NSNotificationName name in @[
                 UIApplicationBackgroundRefreshStatusDidChangeNotification,
                 NSProcessInfoPowerStateDidChangeNotification,
             ]) {
            [center addObserverForName:name
                                object:nil
                                 queue:main
                            usingBlock:^(NSNotification *notification) {
                                (void)notification;
                                flectar_background_refresh_changed();
                            }];
        }
    });
}

// Whether the app is in front of the user. Main thread only.
bool flectar_app_is_active(void) {
    return UIApplication.sharedApplication.applicationState == UIApplicationStateActive;
}

// 0 when iOS checks for mail in the background, 1 when Background App Refresh
// is off for the app or restricted, 2 in Low Power Mode, which turns it off
// for every app. Main thread only.
int flectar_background_refresh_state(void) {
    if (NSProcessInfo.processInfo.isLowPowerModeEnabled) {
        return 2;
    }
    return UIApplication.sharedApplication.backgroundRefreshStatus ==
                   UIBackgroundRefreshStatusAvailable
               ? 0
               : 1;
}

static void flectar_open(NSString *link) {
    dispatch_async(dispatch_get_main_queue(), ^{
        NSURL *url = [NSURL URLWithString:link];
        if (url) {
            [UIApplication.sharedApplication openURL:url options:@{} completionHandler:nil];
        }
    });
}

// The app's notification settings.
void flectar_open_notification_settings(void) {
    flectar_open(UIApplicationOpenNotificationSettingsURLString);
}

// The app's page in Settings, where Background App Refresh is turned on.
void flectar_open_app_settings(void) {
    flectar_open(UIApplicationOpenSettingsURLString);
}
