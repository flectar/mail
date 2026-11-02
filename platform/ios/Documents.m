#import <UIKit/UIKit.h>
#import <Foundation/Foundation.h>

// Imported copies take generated names. Each result carries the name the user
// picked alongside, which Rust validates before using it.
typedef void (*FlectarDocumentCallback)(long long, const char *, const char *, const char *);
typedef void (*FlectarDocumentsCallback)(long long, const char *const *, const char *const *, unsigned long, const char *);
// Accessed only on the main queue. The delegate survives picker dismissal
// while a coordinated background import is still running.
static NSMutableDictionary<NSNumber *, id> *requests;

// One document is copied for the Files pane, whose transfers stop here.
// Several are bounded by the free space alone, which a copy never takes
// below the reserve.
static const unsigned long long singleDocumentLimit = 512ULL * 1024 * 1024;
static const unsigned long long freeSpaceReserve = 64ULL * 1024 * 1024;

static NSError *documentError(NSString *message) {
    return [NSError errorWithDomain:@"FlectarDocuments" code:1
                          userInfo:@{NSLocalizedDescriptionKey: message}];
}

static unsigned long long freeSpace(void) {
    NSNumber *capacity = nil;
    NSURL *home = [NSURL fileURLWithPath:NSHomeDirectory() isDirectory:YES];
    [home getResourceValue:&capacity forKey:NSURLVolumeAvailableCapacityForImportantUsageKey error:nil];
    return capacity ? capacity.unsignedLongLongValue : ULLONG_MAX;
}

@interface FlectarDocumentDelegate : NSObject <UIDocumentPickerDelegate, UIAdaptivePresentationControllerDelegate>
@property(nonatomic) long long requestId;
@property(nonatomic) FlectarDocumentCallback callback;
// Set for a request of several documents, as many as the limit.
@property(nonatomic) FlectarDocumentsCallback documentsCallback;
@property(nonatomic) unsigned long limit;
@property(nonatomic) BOOL exporting;
@property(nonatomic) BOOL finished;
@property(atomic) BOOL cancelled;
@property(nonatomic, strong) UIDocumentPickerViewController *picker;
@end

// Copies the document at `url` into the caches under a generated name, in no
// more than `limit` bytes. Returns the copy and the document's own name, or
// nil and the reason.
static NSURL *copyDocument(FlectarDocumentDelegate *delegate, NSURL *url, unsigned long long limit,
                           NSString **name, NSError **error) {
    BOOL scoped = [url startAccessingSecurityScopedResource];
    __block NSError *readError = nil;
    __block NSString *readName = nil;
    NSError *coordinationError = nil;
    NSURL *folder = [[[NSFileManager defaultManager] URLsForDirectory:NSCachesDirectory
                  inDomains:NSUserDomainMask].firstObject URLByAppendingPathComponent:@"file-imports"
                  isDirectory:YES];
    // The picked name never names a file here.
    NSURL *destination = [folder URLByAppendingPathComponent:NSUUID.UUID.UUIDString];
    NSFileCoordinator *coordinator = [[NSFileCoordinator alloc] initWithFilePresenter:nil];
    [coordinator coordinateReadingItemAtURL:url options:0 error:&coordinationError
                               byAccessor:^(NSURL *readURL) {
        NSNumber *regular = nil;
        [readURL getResourceValue:&regular forKey:NSURLIsRegularFileKey error:&readError];
        if (readError) return;
        if (!regular.boolValue) {
            readError = documentError(@"Select a regular file.");
            return;
        }
        if (delegate.cancelled) { readError = documentError(@"Document transfer cancelled."); return; }
        readName = readURL.lastPathComponent;
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
        unsigned long long checked = 0;
        while (!readError) {
            if (delegate.cancelled) { readError = documentError(@"Document transfer cancelled."); break; }
            NSInteger count = [input read:buffer maxLength:sizeof(buffer)];
            if (count == 0) break;
            if (count < 0) { readError = input.streamError ?: documentError(@"Could not read the document."); break; }
            total += (unsigned long long)count;
            if (total > limit) {
                readError = documentError(@"File exceeds the transfer limit.");
                break;
            }
            // Sizes can be missing or wrong, so the free space is watched too.
            if (total - checked >= 16ULL * 1024 * 1024) {
                checked = total;
                if (freeSpace() < freeSpaceReserve) {
                    readError = documentError(@"There is not enough free space on this device.");
                    break;
                }
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
    NSError *failure = coordinationError ?: readError;
    if (failure || !readName) {
        [[NSFileManager defaultManager] removeItemAtURL:destination error:nil];
        if (error) *error = failure ?: documentError(@"Could not read the document.");
        return nil;
    }
    if (name) *name = readName;
    return destination;
}

@implementation FlectarDocumentDelegate
- (void)finishPaths:(NSArray<NSString *> *)paths names:(NSArray<NSString *> *)names error:(NSString *)error {
    if (self.finished) return;
    self.finished = YES;
    if (self.documentsCallback) {
        NSUInteger count = paths.count;
        // The paths, then a name for each.
        const char **strings = count ? (const char **)calloc(2 * count, sizeof(const char *)) : NULL;
        for (NSUInteger index = 0; index < count; index++) {
            strings[index] = paths[index].UTF8String ?: "";
            strings[count + index] = index < names.count ? (names[index].UTF8String ?: "") : "";
        }
        self.documentsCallback(self.requestId, strings, strings ? strings + count : NULL, (unsigned long)count,
                               error.UTF8String ?: "");
        free(strings);
    } else {
        self.callback(self.requestId, paths.firstObject.UTF8String ?: "", names.firstObject.UTF8String ?: "",
                      error.UTF8String ?: "");
    }
    [requests removeObjectForKey:@(self.requestId)];
    self.picker = nil;
}
- (void)finish:(NSString *)path error:(NSString *)error {
    [self finishPaths:(path.length ? @[path] : @[]) names:@[] error:error];
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
    if (urls.count == 0) { [self finish:@"" error:@"No document was selected."]; return; }
    if (self.exporting) { [self finish:@"exported" error:@""]; return; }
    unsigned long most = self.documentsCallback ? MAX(self.limit, 1UL) : 1UL;
    if (urls.count > most) {
        [self finish:@"" error:[NSString stringWithFormat:@"Choose at most %lu files.", most]];
        return;
    }
    unsigned long long limit = self.documentsCallback ? ULLONG_MAX : singleDocumentLimit;
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        NSMutableArray<NSURL *> *copies = [NSMutableArray array];
        NSMutableArray<NSString *> *names = [NSMutableArray array];
        NSError *error = nil;
        // Sizes known beforehand are checked before anything is copied.
        unsigned long long known = 0;
        for (NSURL *url in urls) {
            BOOL scoped = [url startAccessingSecurityScopedResource];
            NSNumber *size = nil;
            [url getResourceValue:&size forKey:NSURLFileSizeKey error:nil];
            if (scoped) [url stopAccessingSecurityScopedResource];
            known += size.unsignedLongLongValue;
        }
        if (known > limit) {
            error = documentError(@"File exceeds the transfer limit.");
        } else if (known + freeSpaceReserve > freeSpace()) {
            error = documentError(@"There is not enough free space on this device.");
        }
        for (NSURL *url in urls) {
            if (error) break;
            NSString *name = nil;
            NSURL *copy = copyDocument(self, url, limit, &name, &error);
            if (copy) {
                [copies addObject:copy];
                [names addObject:name ?: @""];
            }
        }
        dispatch_async(dispatch_get_main_queue(), ^{
            if (error || self.cancelled || self.finished) {
                for (NSURL *copy in copies) {
                    [[NSFileManager defaultManager] removeItemAtURL:copy error:nil];
                }
                [self finishPaths:@[] names:@[] error:error.localizedDescription ?: @""];
            } else {
                NSMutableArray<NSString *> *paths = [NSMutableArray arrayWithCapacity:copies.count];
                for (NSURL *copy in copies) [paths addObject:copy.path];
                // Rust owns the copies from this callback, even if its
                // receiver was cancelled immediately before delivery.
                [self finishPaths:paths names:names error:@""];
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

// Presents the picker for `delegate`'s request, on the main queue: an export
// of `path`, or an import.
static void presentPicker(FlectarDocumentDelegate *delegate, NSString *path) {
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
    if (!presenter || requests.count) { [delegate finish:@"" error:@"A document picker cannot be opened right now."]; return; }
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
    picker.allowsMultipleSelection = delegate.documentsCallback != NULL;
    delegate.picker = picker;
    requests[@(delegate.requestId)] = delegate;
    [presenter presentViewController:picker animated:YES completion:nil];
    picker.presentationController.delegate = delegate;
}

void flectar_choose_document(long long requestId, const char *exportPath, FlectarDocumentCallback callback) {
    NSString *path = exportPath ? [NSString stringWithUTF8String:exportPath] : @"";
    dispatch_async(dispatch_get_main_queue(), ^{
        FlectarDocumentDelegate *delegate = [FlectarDocumentDelegate new];
        delegate.requestId = requestId;
        delegate.callback = callback;
        presentPicker(delegate, path ?: @"");
    });
}

void flectar_choose_documents(long long requestId, unsigned long limit, FlectarDocumentsCallback callback) {
    dispatch_async(dispatch_get_main_queue(), ^{
        FlectarDocumentDelegate *delegate = [FlectarDocumentDelegate new];
        delegate.requestId = requestId;
        delegate.documentsCallback = callback;
        delegate.limit = limit;
        presentPicker(delegate, @"");
    });
}
