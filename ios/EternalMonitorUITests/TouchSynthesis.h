#import <XCTest/XCTest.h>
#import "DuoHinge.h"

// Compiled into the UI test runner only; never linked into EternalMonitor.app.
void EMSwipeTwoFingers(CGPoint start, CGFloat distance,
                      void (^completion)(BOOL, NSError * _Nullable));
// Every finger touches down and lifts together, at portrait screen points.
void EMTapFingers(NSArray<NSValue *> *points,
                  void (^completion)(BOOL, NSError * _Nullable));
