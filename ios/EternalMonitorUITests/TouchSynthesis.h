#import <XCTest/XCTest.h>

// Compiled into the UI test runner only; never linked into EternalMonitor.app.
void EMSwipeTwoFingers(CGPoint start, CGFloat distance,
                      void (^completion)(BOOL, NSError * _Nullable));
