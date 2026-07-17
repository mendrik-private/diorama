gnome.compile_schemas(build_by_default: true)

install_data(
  'io.github.mendrik.Diorama.gschema.xml',
  install_dir: get_option('datadir') / 'glib-2.0' / 'schemas',
)
install_data(
  'io.github.mendrik.Diorama.desktop.in',
  rename: 'io.github.mendrik.Diorama.desktop',
  install_dir: get_option('datadir') / 'applications',
)
install_data(
  'io.github.mendrik.Diorama.metainfo.xml.in',
  rename: 'io.github.mendrik.Diorama.metainfo.xml',
  install_dir: get_option('datadir') / 'metainfo',
)
install_data(
  'icons/hicolor/scalable/apps/io.github.mendrik.Diorama.svg',
  install_dir: get_option('datadir') / 'icons' / 'hicolor' / 'scalable' / 'apps',
)
