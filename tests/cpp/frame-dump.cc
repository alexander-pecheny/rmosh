/*
    Test harness: build two screens and print the frame that turns one into the other.

    Input is a 4-byte big-endian length, that many bytes fed to the terminal to make the
    "old" screen, then the remaining bytes fed to make the "new" screen. Deriving the
    second screen from the first is what real use does, and it keeps rows shared, which
    the scroll shortcut depends on.

    Exists so the Rust port can be diffed against this implementation. The Rust side has
    an identical tool; see crates/mosh-terminal/tests/.

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
#include "src/terminal/terminaldisplay.h"
#include "src/util/locale_utils.h"

static void feed( Terminal::Emulator& emu, Parser::UTF8Parser& parser, const std::string& bytes )
{
  Parser::Actions actions;
  for ( size_t i = 0; i < bytes.size(); i++ ) {
    actions.clear();
    parser.input( bytes[i], actions );
    for ( Parser::Actions::iterator a = actions.begin(); a != actions.end(); a++ ) {
      ( *a )->act_on_terminal( &emu );
    }
  }
}

int main( int argc, char* argv[] )
{
  set_native_locale();

  int width = 20, height = 6, initialized = 1;
  if ( argc > 3 ) {
    width = atoi( argv[1] );
    height = atoi( argv[2] );
    initialized = atoi( argv[3] );
  }

  std::string all;
  int c;
  while ( ( c = getchar() ) != EOF ) {
    all.push_back( (char)c );
  }
  if ( all.size() < 4 ) {
    return 1;
  }

  size_t split = ( (unsigned char)all[0] << 24 ) | ( (unsigned char)all[1] << 16 )
                 | ( (unsigned char)all[2] << 8 ) | (unsigned char)all[3];
  if ( split > all.size() - 4 ) {
    split = all.size() - 4;
  }

  const std::string first( all, 4, split );
  const std::string second( all, 4 + split );

  Terminal::Emulator emu( width, height );
  Parser::UTF8Parser parser;

  feed( emu, parser, first );
  const Terminal::Framebuffer last( emu.get_fb() );

  feed( emu, parser, second );
  const Terminal::Framebuffer& now = emu.get_fb();

  Terminal::Display display( false );
  const std::string frame = display.new_frame( initialized != 0, last, now );

  fwrite( frame.data(), 1, frame.size(), stdout );
  return 0;
}
