// Camera for the films that show the Windows app.
//
// The window is laid out at the footage's own pixels (2 per app point, under
// a 60px title bar) and only ever scaled down, so the app's text stays sharp.
// pose() says where the camera looks: a point in app points, at `zoom` frame
// pixels per app point, placed at the frame position `at`.
function pose(cx, cy, zoom, at) {
  const s = zoom / 2;
  return { x: at.x - 2 * cx * s, y: at.y - (60 + 2 * cy) * s, scale: s };
}
