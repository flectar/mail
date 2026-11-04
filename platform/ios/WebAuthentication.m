#import <AuthenticationServices/AuthenticationServices.h>
#import <UIKit/UIKit.h>
#include <stdbool.h>

// Gmail and Outlook sign-in for src/platform/ios/oauth.rs. A session shows the
// provider's page in a system sheet that shares Safari's sign-in state and
// returns the redirect to the given custom scheme. Rust tags every session
// with an id and ignores reports for sessions it has given up on.
typedef void (*FlectarWebAuthenticationCallback)(long long id, const char *redirect,
                                                 const char *error, bool cancelled);

@interface FlectarWebAuthenticationAnchor : NSObject <ASWebAuthenticationPresentationContextProviding>
@end

@implementation FlectarWebAuthenticationAnchor
// The sheet attaches to the window the user is looking at.
- (ASPresentationAnchor)presentationAnchorForWebAuthenticationSession:(ASWebAuthenticationSession *)session {
    UIWindowScene *fallback = nil;
    for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
        if (![scene isKindOfClass:UIWindowScene.class]) continue;
        UIWindowScene *windowScene = (UIWindowScene *)scene;
        if (scene.activationState == UISceneActivationStateForegroundActive && windowScene.keyWindow) {
            return windowScene.keyWindow;
        }
        if (!fallback) fallback = windowScene;
    }
    UIWindow *window = fallback.keyWindow ?: fallback.windows.firstObject;
    if (window) return window;
    return fallback ? [[UIWindow alloc] initWithWindowScene:fallback] : [[UIWindow alloc] init];
}
@end

// Accessed only on the main queue.
static ASWebAuthenticationSession *currentSession;
static long long currentId;
static FlectarWebAuthenticationAnchor *anchor;

void flectar_start_web_authentication(long long requestId, const char *url, const char *scheme,
                                      FlectarWebAuthenticationCallback callback) {
    NSURL *address = [NSURL URLWithString:[NSString stringWithUTF8String:url]];
    NSString *callbackScheme = [NSString stringWithUTF8String:scheme];
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!address || callbackScheme.length == 0) {
            callback(requestId, NULL, "The sign-in address is invalid.", false);
            return;
        }
        // Rust starts one sign-in at a time; a session still showing belongs
        // to one it has given up on.
        [currentSession cancel];
        void (^completion)(NSURL *, NSError *) = ^(NSURL *redirect, NSError *error) {
            if (currentId == requestId) {
                currentSession = nil;
            }
            if (redirect) {
                callback(requestId, redirect.absoluteString.UTF8String, NULL, false);
                return;
            }
            bool cancelled = [error.domain isEqualToString:ASWebAuthenticationSessionErrorDomain] &&
                             error.code == ASWebAuthenticationSessionErrorCodeCanceledLogin;
            NSString *message = error.localizedDescription ?: @"The sign-in page closed.";
            callback(requestId, NULL, message.UTF8String, cancelled);
        };
        ASWebAuthenticationSession *session;
        // Callbacks arrived in iOS 17.4. `@available` would need compiler-rt's
        // __isPlatformVersionAtLeast, which rustc doesn't link, so look the
        // class up instead.
        Class callbacks = NSClassFromString(@"ASWebAuthenticationSessionCallback");
        if (callbacks) {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunguarded-availability-new"
            session = [[ASWebAuthenticationSession alloc]
                      initWithURL:address
                         callback:[callbacks callbackWithCustomScheme:callbackScheme]
                completionHandler:completion];
#pragma clang diagnostic pop
        } else {
            session = [[ASWebAuthenticationSession alloc] initWithURL:address
                                                    callbackURLScheme:callbackScheme
                                                    completionHandler:completion];
        }
        if (!anchor) anchor = [[FlectarWebAuthenticationAnchor alloc] init];
        session.presentationContextProvider = anchor;
        currentSession = session;
        currentId = requestId;
        if (![session start]) {
            currentSession = nil;
            callback(requestId, NULL, "Could not show the sign-in page.", false);
        }
    });
}

void flectar_cancel_web_authentication(long long requestId) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (currentId == requestId) {
            [currentSession cancel];
            currentSession = nil;
        }
    });
}
