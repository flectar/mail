@Entry
@Component
struct Index {
  build() {
    Column() {
      XComponent({
        id: 'slint_demo_surface',
        type: XComponentType.SURFACE,
        libraryname: 'slint_ohos_demo',
      })
        .width('100%')
        .height('100%')
        .focusable(true)
        .defaultFocus(true)
    }
    .width('100%')
    .height('100%')
  }
}
