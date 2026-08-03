/*
    Test harness: read bytes on stdin, print the action stream the parser produces.

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

#include "src/terminal/parser.h"
#include "src/util/locale_utils.h"

int main( void )
{
  set_native_locale();

  Parser::UTF8Parser parser;
  Parser::Actions actions;

  int c;
  while ( ( c = getchar() ) != EOF ) {
    actions.clear();
    parser.input( (char)c, actions );
    for ( Parser::Actions::iterator i = actions.begin(); i != actions.end(); i++ ) {
      printf( "%s", ( *i )->name().c_str() );
      if ( ( *i )->char_present ) {
        printf( " %u", (unsigned int)( *i )->ch );
      }
      printf( "\n" );
    }
  }

  return 0;
}
