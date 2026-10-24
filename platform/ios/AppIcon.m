#import <UIKit/UIKit.h>

// Show the named alternate icon from the asset catalog, or the primary icon
// for NULL. iOS confirms each change with its own alert and refuses changes
// while the app is not active, so only a real change is requested.
void flectar_set_app_icon(const char *name) {
    NSString *icon = name ? [NSString stringWithUTF8String:name] : nil;
    dispatch_async(dispatch_get_main_queue(), ^{
        UIApplication *application = UIApplication.sharedApplication;
        if (!application.supportsAlternateIcons) return;
        NSString *current = application.alternateIconName;
        if (current == icon || [current isEqualToString:icon]) return;
        if (application.applicationState != UIApplicationStateActive) return;
        [application setAlternateIconName:icon completionHandler:^(NSError *error) {
            if (error) NSLog(@"could not change the app icon: %@", error);
        }];
    });
}
