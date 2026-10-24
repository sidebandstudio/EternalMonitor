#import <Foundation/Foundation.h>

// Compiled into the UI test runner only; never linked into EternalMonitor.app.
// The iPhone Duo simulator's hinge, set the way Xcode's Device Hub sets it.
BOOL EMHingeAvailable(void);
// Sets the hinge to `degrees` (0 closed, 180 flat) and waits up to `timeout`
// seconds for the simulator to report that angle.
BOOL EMSetHingeAngle(double degrees, NSTimeInterval timeout, NSString * _Nullable * _Nullable error);
// XCUIDevice can't rotate the iPhone Duo simulator; Device Hub's orientation
// picker can. `orientation` is portrait, landscape-left, landscape-right or pud.
BOOL EMSetDeviceOrientation(NSString *orientation, NSString * _Nullable * _Nullable error);
