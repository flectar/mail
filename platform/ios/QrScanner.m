#import <AVFoundation/AVFoundation.h>
#import <UIKit/UIKit.h>

// Scans a QR code with the camera, to pair devices in Flectar Mail Sync. iOS
// has no scanner an app can use without camera access, so iOS asks for it the
// first time. The camera only reads the code: no picture is kept or sent.

typedef void (*FlectarQrCallback)(const char *text, int outcome);

// What a scan ended with, for Rust (src/platform/ios/qr_scanner.rs).
enum {
    FlectarQrCode = 0,
    FlectarQrClosed = 1,
    FlectarQrDenied = 2,
    FlectarQrFailed = 3,
};

@interface FlectarQrScanner : UIViewController <AVCaptureMetadataOutputObjectsDelegate>
@property(nonatomic) FlectarQrCallback callback;
@property(nonatomic, copy) NSString *hint;
@property(nonatomic, copy) NSString *close;
@property(nonatomic, strong) AVCaptureSession *session;
@property(nonatomic, strong) AVCaptureVideoPreviewLayer *preview;
@property(nonatomic) BOOL finished;
@end

// The scanner on screen, accessed only on the main queue.
static FlectarQrScanner *current;

@implementation FlectarQrScanner

- (void)viewDidLoad {
    [super viewDidLoad];
    self.view.backgroundColor = UIColor.blackColor;

    AVCaptureDevice *camera = [AVCaptureDevice defaultDeviceWithMediaType:AVMediaTypeVideo];
    NSError *error = nil;
    AVCaptureDeviceInput *input = camera ? [AVCaptureDeviceInput deviceInputWithDevice:camera error:&error] : nil;
    AVCaptureMetadataOutput *output = [AVCaptureMetadataOutput new];
    AVCaptureSession *session = [AVCaptureSession new];
    if (!input || ![session canAddInput:input] || ![session canAddOutput:output]) {
        [self finish:nil outcome:FlectarQrFailed];
        return;
    }
    [session addInput:input];
    [session addOutput:output];
    [output setMetadataObjectsDelegate:self queue:dispatch_get_main_queue()];
    output.metadataObjectTypes = @[AVMetadataObjectTypeQRCode];
    self.session = session;

    self.preview = [AVCaptureVideoPreviewLayer layerWithSession:session];
    self.preview.videoGravity = AVLayerVideoGravityResizeAspectFill;
    [self.view.layer addSublayer:self.preview];

    UILabel *hint = [UILabel new];
    hint.text = self.hint;
    hint.textColor = UIColor.whiteColor;
    hint.font = [UIFont preferredFontForTextStyle:UIFontTextStyleHeadline];
    hint.adjustsFontForContentSizeCategory = YES;
    hint.numberOfLines = 0;
    hint.textAlignment = NSTextAlignmentCenter;
    hint.translatesAutoresizingMaskIntoConstraints = NO;
    [self.view addSubview:hint];

    UIButton *close = [UIButton buttonWithType:UIButtonTypeSystem];
    [close setTitle:self.close forState:UIControlStateNormal];
    [close setTitleColor:UIColor.whiteColor forState:UIControlStateNormal];
    close.titleLabel.font = [UIFont preferredFontForTextStyle:UIFontTextStyleBody];
    close.titleLabel.adjustsFontForContentSizeCategory = YES;
    close.translatesAutoresizingMaskIntoConstraints = NO;
    [close addTarget:self action:@selector(closeTapped) forControlEvents:UIControlEventTouchUpInside];
    [self.view addSubview:close];

    UILayoutGuide *safe = self.view.safeAreaLayoutGuide;
    [NSLayoutConstraint activateConstraints:@[
        [hint.leadingAnchor constraintEqualToAnchor:safe.leadingAnchor constant:24],
        [hint.trailingAnchor constraintEqualToAnchor:safe.trailingAnchor constant:-24],
        [hint.topAnchor constraintEqualToAnchor:safe.topAnchor constant:24],
        [close.centerXAnchor constraintEqualToAnchor:safe.centerXAnchor],
        [close.bottomAnchor constraintEqualToAnchor:safe.bottomAnchor constant:-24],
        [close.heightAnchor constraintGreaterThanOrEqualToConstant:44],
    ]];
}

- (void)viewDidLayoutSubviews {
    [super viewDidLayoutSubviews];
    self.preview.frame = self.view.bounds;
}

- (void)viewWillAppear:(BOOL)animated {
    [super viewWillAppear:animated];
    AVCaptureSession *session = self.session;
    // startRunning blocks until the camera is on.
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        [session startRunning];
    });
}

- (void)closeTapped {
    [self finish:nil outcome:FlectarQrClosed];
}

- (void)captureOutput:(AVCaptureOutput *)output
    didOutputMetadataObjects:(NSArray<__kindof AVMetadataObject *> *)objects
              fromConnection:(AVCaptureConnection *)connection {
    for (AVMetadataObject *object in objects) {
        if ([object isKindOfClass:AVMetadataMachineReadableCodeObject.class]) {
            NSString *text = ((AVMetadataMachineReadableCodeObject *)object).stringValue;
            if (text) {
                [self finish:text outcome:FlectarQrCode];
                return;
            }
        }
    }
}

// Answers Rust once, and closes the scanner.
- (void)finish:(NSString *)text outcome:(int)outcome {
    if (self.finished) return;
    self.finished = YES;
    AVCaptureSession *session = self.session;
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        [session stopRunning];
    });
    FlectarQrCallback callback = self.callback;
    NSString *answer = [text copy];
    UIViewController *presenter = self.presentingViewController;
    void (^answerRust)(void) = ^{
        if (current == self) current = nil;
        callback(answer.UTF8String, outcome);
    };
    if (presenter) {
        [presenter dismissViewControllerAnimated:YES completion:answerRust];
    } else {
        answerRust();
    }
}

@end

static UIViewController *presentingController(void) {
    UIWindow *window = nil;
    for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
        if (scene.activationState == UISceneActivationStateForegroundActive && [scene isKindOfClass:UIWindowScene.class]) {
            for (UIWindow *candidate in ((UIWindowScene *)scene).windows) {
                if (candidate.isKeyWindow) { window = candidate; break; }
            }
        }
        if (window) break;
    }
    UIViewController *presenter = window.rootViewController;
    while (presenter.presentedViewController) presenter = presenter.presentedViewController;
    return presenter;
}

static void present(NSString *hint, NSString *close, FlectarQrCallback callback) {
    UIViewController *presenter = presentingController();
    if (!presenter || current) {
        callback("The scanner can't open right now.", FlectarQrFailed);
        return;
    }
    FlectarQrScanner *scanner = [FlectarQrScanner new];
    scanner.callback = callback;
    scanner.hint = hint;
    scanner.close = close;
    scanner.modalPresentationStyle = UIModalPresentationFullScreen;
    current = scanner;
    [presenter presentViewController:scanner animated:YES completion:nil];
}

bool flectar_qr_can_scan(void) {
    return [AVCaptureDevice defaultDeviceWithMediaType:AVMediaTypeVideo] != nil;
}

void flectar_qr_scan(const char *hint, const char *close, FlectarQrCallback callback) {
    NSString *hintText = hint ? [NSString stringWithUTF8String:hint] : @"";
    NSString *closeText = close ? [NSString stringWithUTF8String:close] : @"";
    dispatch_async(dispatch_get_main_queue(), ^{
        AVAuthorizationStatus status = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeVideo];
        if (status == AVAuthorizationStatusAuthorized) {
            present(hintText, closeText, callback);
        } else if (status == AVAuthorizationStatusNotDetermined) {
            [AVCaptureDevice requestAccessForMediaType:AVMediaTypeVideo completionHandler:^(BOOL granted) {
                dispatch_async(dispatch_get_main_queue(), ^{
                    if (granted) {
                        present(hintText, closeText, callback);
                    } else {
                        callback(NULL, FlectarQrDenied);
                    }
                });
            }];
        } else {
            callback(NULL, FlectarQrDenied);
        }
    });
}
