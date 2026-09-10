#import <UIKit/UIKit.h>
#import <Foundation/Foundation.h>

typedef void (*FlectarDocumentCallback)(long long, const char *, const char *);
// Accessed only on the main queue. The delegate survives picker dismissal
// while a coordinated background import is still running.
static NSMutableDictionary<NSNumber *, id> *requests;

static NSError *documentError(NSString *message) {
    return [NSError errorWithDomain:@"FlectarDocuments" code:1
                          userInfo:@{NSLocalizedDescriptionKey: message}];
}

@interface FlectarDocumentDelegate : NSObject <UIDocumentPickerDelegate, UIAdaptivePresentationControllerDelegate>
@property(nonatomic) long long requestId;
@property(nonatomic) FlectarDocumentCallback callback;
@property(nonatomic) BOOL exporting;
@property(nonatomic) BOOL finished;
@property(atomic) BOOL cancelled;
@property(nonatomic, strong) UIDocumentPickerViewController *picker;
@end

@implementation FlectarDocumentDelegate
- (void)finish:(NSString *)path error:(NSString *)error {
    if (self.finished) return;
    self.finished = YES;
    self.callback(self.requestId, path.UTF8String ?: "", error.UTF8String ?: "");
    [requests removeObjectForKey:@(self.requestId)];
    self.picker = nil;
}
- (void)documentPickerWasCancelled:(UIDocumentPickerViewController *)controller {
    self.cancelled = YES;
    [self finish:@"" error:@""];
}
- (void)presentationControllerDidDismiss:(UIPresentationController *)controller {
    self.cancelled = YES;
    [self finish:@"" error:@""];
}
- (void)documentPicker:(UIDocumentPickerViewController *)controller didPickDocumentsAtURLs:(NSArray<NSURL *> *)urls {
    NSURL *url = urls.firstObject;
    if (!url) { [self finish:@"" error:@"No document was selected."]; return; }
    if (self.exporting) { [self finish:@"exported" error:@""]; return; }
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        BOOL scoped = [url startAccessingSecurityScopedResource];
        __block NSError *readError = nil;
        NSError *coordinationError = nil;
        __block NSURL *destination = nil;
        NSURL *folder = [[[[NSFileManager defaultManager] URLsForDirectory:NSCachesDirectory
                      inDomains:NSUserDomainMask].firstObject URLByAppendingPathComponent:@"file-imports"
                      isDirectory:YES] URLByAppendingPathComponent:NSUUID.UUID.UUIDString isDirectory:YES];
        NSFileCoordinator *coordinator = [[NSFileCoordinator alloc] initWithFilePresenter:nil];
        [coordinator coordinateReadingItemAtURL:url options:0 error:&coordinationError
                                   byAccessor:^(NSURL *readURL) {
            NSNumber *regular = nil;
            [readURL getResourceValue:&regular forKey:NSURLIsRegularFileKey error:&readError];
            NSString *name = readURL.lastPathComponent;
            if (readError) return;
            if (!regular.boolValue || name.length == 0 || [name isEqualToString:@"."] ||
                [name isEqualToString:@".."] || [name containsString:@"/"] || [name containsString:@"\\"]) {
                readError = documentError(@"Select a regular file with a valid name.");
                return;
            }
            if (self.cancelled) { readError = documentError(@"Document transfer cancelled."); return; }
            destination = [folder URLByAppendingPathComponent:name];
            [[NSFileManager defaultManager] createDirectoryAtURL:folder withIntermediateDirectories:YES
                                    attributes:@{NSFilePosixPermissions: @0700} error:&readError];
            if (readError) return;
            NSInputStream *input = [NSInputStream inputStreamWithURL:readURL];
            NSOutputStream *output = [NSOutputStream outputStreamWithURL:destination append:NO];
            if (!input || !output) { readError = documentError(@"Could not open the document streams."); return; }
            [input open];
            [output open];
            uint8_t buffer[65536];
            unsigned long long total = 0;
            while (!readError) {
                if (self.cancelled) { readError = documentError(@"Document transfer cancelled."); break; }
                NSInteger count = [input read:buffer maxLength:sizeof(buffer)];
                if (count == 0) break;
                if (count < 0) { readError = input.streamError ?: documentError(@"Could not read the document."); break; }
                total += (unsigned long long)count;
                if (total > 512ULL * 1024 * 1024) {
                    readError = documentError(@"File exceeds the transfer limit.");
                    break;
                }
                NSInteger offset = 0;
                while (offset < count) {
                    NSInteger written = [output write:buffer + offset maxLength:(NSUInteger)(count - offset)];
                    if (written <= 0) { readError = output.streamError ?: documentError(@"Could not write the imported file."); break; }
                    offset += written;
                }
            }
            [input close];
            [output close];
        }];
        if (scoped) [url stopAccessingSecurityScopedResource];
        NSError *error = coordinationError ?: readError;
        dispatch_async(dispatch_get_main_queue(), ^{
            if (error || self.cancelled || self.finished || !destination) {
                [[NSFileManager defaultManager] removeItemAtURL:folder error:nil];
                [self finish:@"" error:error.localizedDescription ?: @""];
            } else {
                // Rust owns the copy from this callback, even if its receiver
                // was cancelled immediately before delivery.
                [self finish:destination.path error:@""];
            }
        });
    });
}
@end

void flectar_cancel_document(long long requestId) {
    dispatch_async(dispatch_get_main_queue(), ^{
        FlectarDocumentDelegate *delegate = requests[@(requestId)];
        if (!delegate) return;
        delegate.cancelled = YES;
        [delegate.picker dismissViewControllerAnimated:YES completion:nil];
        [delegate finish:@"" error:@""];
    });
}

void flectar_choose_document(long long requestId, const char *exportPath, FlectarDocumentCallback callback) {
    NSString *path = exportPath ? [NSString stringWithUTF8String:exportPath] : @"";
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!requests) requests = [NSMutableDictionary dictionary];
        UIWindow *window = nil;
        if (@available(iOS 13.0, *)) {
            for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
                if (scene.activationState == UISceneActivationStateForegroundActive && [scene isKindOfClass:UIWindowScene.class]) {
                    for (UIWindow *candidate in ((UIWindowScene *)scene).windows) {
                        if (candidate.isKeyWindow) { window = candidate; break; }
                    }
                }
                if (window) break;
            }
        }
        if (!window) window = UIApplication.sharedApplication.keyWindow;
        UIViewController *presenter = window.rootViewController;
        while (presenter.presentedViewController) presenter = presenter.presentedViewController;
        if (!presenter || requests.count) { callback(requestId, "", "A document picker cannot be opened right now."); return; }
        FlectarDocumentDelegate *delegate = [FlectarDocumentDelegate new];
        delegate.requestId = requestId;
        delegate.callback = callback;
        delegate.exporting = path.length > 0;
        UIDocumentPickerViewController *picker;
        if (delegate.exporting) {
            picker = [[UIDocumentPickerViewController alloc] initWithURL:[NSURL fileURLWithPath:path]
                                                                 inMode:UIDocumentPickerModeExportToService];
        } else {
            picker = [[UIDocumentPickerViewController alloc] initWithDocumentTypes:@[@"public.data", @"public.content"]
                                                                           inMode:UIDocumentPickerModeImport];
        }
        picker.delegate = delegate;
        picker.allowsMultipleSelection = NO;
        delegate.picker = picker;
        requests[@(requestId)] = delegate;
        [presenter presentViewController:picker animated:YES completion:nil];
        picker.presentationController.delegate = delegate;
    });
}
