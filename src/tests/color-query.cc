/*
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

    In addition, as a special exception, the copyright holders give
    permission to link the code of portions of this program with the
    OpenSSL library under certain conditions as described in each
    individual source file, and distribute linked combinations including
    the two.

    You must obey the GNU General Public License in all respects for all
    of the code used other than OpenSSL. If you modify file(s) with this
    exception, you may extend this exception to your version of the
    file(s), but you are not obligated to do so. If you do not wish to do
    so, delete this exception statement from your version. If you delete
    this exception statement from all source files in the program, then
    also delete it here.
*/

#include <cstdio>
#include <cstdlib>
#include <string>

#include "src/statesync/completeterminal.h"
#include "src/terminal/terminaldisplay.h"

static const int WIDTH = 80;
static const int HEIGHT = 24;

static void expect( bool condition, const char* description )
{
  if ( !condition ) {
    fprintf( stderr, "FAILED: %s\n", description );
    exit( 1 );
  }
}

static bool contains( const std::string& haystack, const std::string& needle )
{
  return haystack.find( needle ) != std::string::npos;
}

int main( void )
{
  const std::string background_query( "\033]11;?\033\\" );
  const std::string palette_query( "\033]4;1;?\033\\" );

  Terminal::Display display( false );
  Terminal::Framebuffer blank( WIDTH, HEIGHT );
  Terminal::Complete complete( WIDTH, HEIGHT );

  complete.act( background_query + palette_query );
  expect( contains( complete.get_fb().get_color_queries(), background_query ), "background query is pending" );
  expect( contains( complete.get_fb().get_color_queries(), palette_query ), "palette query is pending" );

  const std::string frame = display.new_frame( true, blank, complete.get_fb() );
  expect( contains( frame, background_query ), "background query reaches the host terminal" );
  expect( contains( frame, palette_query ), "palette query reaches the host terminal" );

  const std::string repeat = display.new_frame( true, complete.get_fb(), complete.get_fb() );
  expect( !contains( repeat, background_query ), "an unchanged frame does not repeat the query" );

  const std::string attach = display.new_frame( false, blank, complete.get_fb() );
  expect( !contains( attach, background_query ), "a fresh client does not replay a stale query" );

  complete.act( "hello" );
  expect( complete.get_fb().get_color_queries().empty(), "later output clears answered queries" );

  complete.act( "\033]11;rgb:1e1e/1e1e/2e2e\033\\" );
  expect( complete.get_fb().get_color_queries().empty(), "a color setting is not forwarded" );

  complete.act( "\033]52;c;?\033\\" );
  expect( complete.get_fb().get_color_queries().empty(), "a clipboard query is not a color query" );

  printf( "PASS\n" );
  return 0;
}
