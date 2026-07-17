project(
  'diorama',
  'rust',
  version: '0.1.0',
  meson_version: '>= 1.3.0',
  default_options: ['warning_level=2'],
)

gnome = import('gnome')
cargo_script = find_program('build-aux/cargo-build.sh')

diorama = custom_target(
  'diorama',
  output: 'diorama',
  command: [cargo_script, meson.project_source_root(), '@OUTPUT@', get_option('buildtype')],
  build_by_default: true,
  install: true,
  install_dir: get_option('bindir'),
)

subdir('data')

test('cargo-tests', cargo_script, args: [meson.project_source_root(), '', 'test'])
