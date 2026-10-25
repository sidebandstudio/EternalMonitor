#import "TouchSynthesis.h"
#import <UIKit/UIKit.h>

// XCTest exposes taps with multiple touches but no public two-finger swipe.
// Declare only the runtime selectors used to drive actual UIKit touch events.
@interface NSObject (EMTouchEvents)
- (id)initForTouchAtPoint:(CGPoint)point offset:(double)offset;
- (void)moveToPoint:(CGPoint)point atOffset:(double)offset;
- (void)liftUpAtOffset:(double)offset;
- (id)initWithName:(NSString *)name interfaceOrientation:(NSInteger)orientation;
- (id)initWithName:(NSString *)name displayID:(unsigned int)displayID interfaceOrientation:(NSInteger)orientation;
- (unsigned int)displayID;
- (NSInteger)interfaceOrientation;
- (void)_dispatchEvent:(NSString *)name eventBuilder:(id (^)(id target))builder;
- (void)addPointerEventPath:(id)path;
- (id)eventSynthesizer;
- (void)synthesizeEvent:(id)record completion:(void (^)(BOOL, NSError *))completion;
@end

void EMSwipeTwoFingers(CGPoint start, CGFloat distance,
                      void (^completion)(BOOL, NSError *)) {
    Class pathClass = NSClassFromString(@"XCPointerEventPath");
    Class recordClass = NSClassFromString(@"XCSynthesizedEventRecord");
    if (![pathClass instancesRespondToSelector:@selector(initForTouchAtPoint:offset:)] ||
        ![recordClass instancesRespondToSelector:@selector(initWithName:interfaceOrientation:)] ||
        ![XCUIDevice.sharedDevice respondsToSelector:@selector(eventSynthesizer)]) {
        completion(NO, [NSError errorWithDomain:@"EMTouchSynthesis" code:1 userInfo:@{
            NSLocalizedDescriptionKey: @"This XCTest runtime cannot synthesize two touch paths"}]);
        return;
    }
    id record = [[recordClass alloc] initWithName:@"Two-finger upward scroll"
                           interfaceOrientation:UIInterfaceOrientationPortrait];
    for (NSInteger finger = 0; finger < 2; finger++) {
        CGPoint point = CGPointMake(start.x + (finger == 0 ? -20 : 20), start.y);
        id path = [[pathClass alloc] initForTouchAtPoint:point offset:0];
        for (NSInteger step = 1; step <= 6; step++) {
            [path moveToPoint:CGPointMake(point.x, point.y - distance * step / 6)
                    atOffset:step * 0.1];
        }
        [path liftUpAtOffset:0.7];
        [record addPointerEventPath:path];
    }
    [[XCUIDevice.sharedDevice eventSynthesizer] synthesizeEvent:record completion:completion];
}

void EMTapFingers(NSArray<NSValue *> *points,
                  void (^completion)(BOOL, NSError *)) {
    Class pathClass = NSClassFromString(@"XCPointerEventPath");
    Class recordClass = NSClassFromString(@"XCSynthesizedEventRecord");
    if (![pathClass instancesRespondToSelector:@selector(initForTouchAtPoint:offset:)] ||
        ![recordClass instancesRespondToSelector:@selector(initWithName:interfaceOrientation:)] ||
        ![XCUIDevice.sharedDevice respondsToSelector:@selector(eventSynthesizer)]) {
        completion(NO, [NSError errorWithDomain:@"EMTouchSynthesis" code:1 userInfo:@{
            NSLocalizedDescriptionKey: @"This XCTest runtime cannot synthesize a multi-finger tap"}]);
        return;
    }
    id record = [[recordClass alloc] initWithName:@"Multi-finger tap"
                           interfaceOrientation:UIInterfaceOrientationPortrait];
    for (NSValue *point in points) {
        id path = [[pathClass alloc] initForTouchAtPoint:point.CGPointValue offset:0];
        [path liftUpAtOffset:0.1];
        [record addPointerEventPath:path];
    }
    [[XCUIDevice.sharedDevice eventSynthesizer] synthesizeEvent:record completion:completion];
}

BOOL EMTapFingersFromElement(XCUIElement *element, NSArray<NSValue *> *offsets, NSString **error) {
    Class pathClass = NSClassFromString(@"XCPointerEventPath");
    Class recordClass = NSClassFromString(@"XCSynthesizedEventRecord");
    if (![pathClass instancesRespondToSelector:@selector(initForTouchAtPoint:offset:)] ||
        ![recordClass instancesRespondToSelector:@selector(initWithName:displayID:interfaceOrientation:)] ||
        ![element respondsToSelector:@selector(_dispatchEvent:eventBuilder:)] ||
        ![element respondsToSelector:@selector(displayID)] ||
        ![element respondsToSelector:@selector(interfaceOrientation)]) {
        if (error) *error = @"This XCTest runtime cannot tap with several fingers on an element's own screen";
        return NO;
    }
    // Built as XCUICoordinate's tap builds its one touch, once the element
    // is resolved: screen points from its frame, on its display, in its
    // orientation.
    [(NSObject *)element _dispatchEvent:@"Multi-finger tap on its own screen" eventBuilder:^id(id target) {
        NSObject *source = (NSObject *)element;
        id record = [[recordClass alloc] initWithName:@"Multi-finger tap"
                                            displayID:source.displayID
                                 interfaceOrientation:source.interfaceOrientation];
        XCUICoordinate *origin = [element coordinateWithNormalizedOffset:CGVectorMake(0, 0)];
        for (NSValue *offset in offsets) {
            CGPoint point = [origin coordinateWithOffset:CGVectorMake(offset.CGPointValue.x, offset.CGPointValue.y)].screenPoint;
            id path = [[pathClass alloc] initForTouchAtPoint:point offset:0];
            [path liftUpAtOffset:0.1];
            [record addPointerEventPath:path];
        }
        return record;
    }];
    return YES;
}
