/*
    Test harness: read bytes on stdin, print the resulting screen state.

    Exists so the Rust port can be diffed against this implementation over arbitrary
    input. The Rust side has an identical tool; see crates/mosh-terminal/tests/.

    Mosh: the mobile shell
    Copyright 2012 Keith Winstein

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.

    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU General Public License for more details.

    You should have received a copy of the GNU General Public License
    along with this program.  If not, see <http://www.gnu.org/licenses/>.
*/

#include <cstdio>
#include <cstdlib>
#include <string>

#include "src/terminal/parser.h"
#include "src/terminal/terminal.h"
#include "src/util/locale_utils.h"

int main( int argc, char* argv[] )
{
  set_native_locale();

  int width = 20, height = 6;
  if ( argc > 2 ) {
    width = atoi( argv[1] );
    height = atoi( argv[2] );
  }

  Terminal::Emulator emu( width, height );
  Parser::UTF8Parser parser;
  Parser::Actions actions;

  int c;
  while ( ( c = getchar() ) != EOF ) {
    actions.clear();
    parser.input( (char)c, actions );
    for ( Parser::Actions::iterator i = actions.begin(); i != actions.end(); i++ ) {
      ( *i )->act_on_terminal( &emu );
    }
  }

  const Terminal::Framebuffer& fb = emu.get_fb();

  printf( "cursor %d %d\n", fb.ds.get_cursor_row(), fb.ds.get_cursor_col() );
  printf( "visible %d\n", (int)fb.ds.cursor_visible );
  printf( "reverse %d\n", (int)fb.ds.reverse_video );
  printf( "origin %d\n", (int)fb.ds.origin_mode );
  printf( "autowrap %d\n", (int)fb.ds.auto_wrap_mode );
  printf( "insert %d\n", (int)fb.ds.insert_mode );
  printf( "bracketed %d\n", (int)fb.ds.bracketed_paste );
  printf( "appcursor %d\n", (int)fb.ds.application_mode_cursor_keys );
  printf( "region %d %d\n", fb.ds.get_scrolling_region_top_row(), fb.ds.get_scrolling_region_bottom_row() );
  printf( "bell %u\n", fb.get_bell_count() );
  printf( "sgr %s\n", fb.ds.get_renditions().sgr().c_str() );

  for ( int y = 0; y < fb.ds.get_height(); y++ ) {
    for ( int x = 0; x < fb.ds.get_width(); x++ ) {
      const Terminal::Cell* cell = fb.get_cell( y, x );
      std::string grapheme;
      cell->print_grapheme( grapheme );
      printf( "cell %d %d [%s] %s w%d f%d r%d\n",
              y,
              x,
              grapheme.c_str(),
              cell->get_renditions().sgr().c_str(),
              (int)cell->get_wide(),
              (int)cell->get_fallback(),
              (int)cell->get_wrap() );
    }
  }

  return 0;
}
