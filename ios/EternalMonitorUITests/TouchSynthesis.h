#import <XCTest/XCTest.h>
#import "DuoHinge.h"

// Compiled into the UI test runner only; never linked into EternalMonitor.app.
void EMSwipeTwoFingers(CGPoint start, CGFloat distance,
                      void (^completion)(BOOL, NSError * _Nullable));
// Every finger touches down and lifts together, at portrait screen points.
void EMTapFingers(NSArray<NSValue *> *points,
                  void (^completion)(BOOL, NSError * _Nullable));
// Every finger touches down and lifts together, `offsets` points from the
// element's top left, sent the way XCUICoordinate sends a tap: to the
// element's display, in its interface orientation. XCUIElement's own
// multi-finger tap looks for the element in the wrong space on the iPhone
// Duo's inner screen and fails.
BOOL EMTapFingersFromElement(XCUIElement * _Nonnull element, NSArray<NSValue *> * _Nonnull offsets,
                             NSString * _Nullable * _Nullable error);
