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
// Before each event XCTest waits up to 60 s for SpringBoard's animations to
// finish. After some folds the iPhone Duo simulator's SpringBoard never says
// they did, so every touch then comes a minute late, after the HUD it was
// meant for has hidden. While `skip` is set, XCTest still waits for
// SpringBoard to idle but not for its animations; the tests wait for the
// app themselves.
BOOL EMSkipSpringBoardAnimationWait(BOOL skip, NSString * _Nullable * _Nullable error);
