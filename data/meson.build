gnome.compile_schemas(build_by_default: true)

install_data(
  'io.github.mendrik.Diorama.gschema.xml',
  install_dir: get_option('datadir') / 'glib-2.0' / 'schemas',
)
i18n.merge_file(
  input: 'io.github.mendrik.Diorama.desktop.in',
  output: 'io.github.mendrik.Diorama.desktop',
  type: 'desktop',
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'applications',
)
i18n.merge_file(
  input: 'io.github.mendrik.Diorama.metainfo.xml.in',
  output: 'io.github.mendrik.Diorama.metainfo.xml',
  type: 'xml',
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'metainfo',
)
install_data(
  'icons/hicolor/scalable/apps/io.github.mendrik.Diorama.svg',
  install_dir: get_option('datadir') / 'icons' / 'hicolor' / 'scalable' / 'apps',
)
install_data(
  'icons/hicolor/scalable/actions/ruler-measure-symbolic.svg',
  install_dir: get_option('datadir') / 'icons' / 'hicolor' / 'scalable' / 'actions',
)
install_data(
  'icons/hicolor/scalable/actions/pencil-symbolic.svg',
  'icons/hicolor/scalable/actions/highlight-symbolic.svg',
  'icons/hicolor/scalable/actions/arrow-symbolic.svg',
  'icons/hicolor/scalable/actions/text-symbolic.svg',
  install_dir: get_option('datadir') / 'icons' / 'hicolor' / 'scalable' / 'actions',
)
install_data(
  'fonts/Excalifont-Regular.ttf',
  install_dir: get_option('datadir') / 'fonts' / 'truetype' / 'diorama',
)
install_data(
  'fonts/OFL-Excalifont.txt',
  install_dir: get_option('datadir') / 'doc' / meson.project_name(),
)
