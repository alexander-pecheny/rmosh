/*
    Test harness: decrypt and unwrap one datagram, printing what each layer holds.

    Takes the session key as its argument and the datagram on stdin. Exists so the Rust
    port can prove that a datagram it builds is readable by this implementation -- the
    substance of the backwards-compatibility requirement.

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
#include <string>

#include "src/crypto/crypto.h"
#include "src/network/network.h"
#include "src/network/transportfragment.h"

int main( int argc, char* argv[] )
{
  if ( argc != 2 ) {
    fprintf( stderr, "Usage: %s KEY\n", argv[0] );
    return 1;
  }

  std::string input;
  int c;
  while ( ( c = getchar() ) != EOF ) {
    input.push_back( (char)c );
  }

  try {
    Crypto::Base64Key key( argv[1] );
    Crypto::Session session( key );

    Crypto::Message message = session.decrypt( input );
    Network::Packet packet( message );

    printf( "seq %llu\n", (unsigned long long)packet.seq );
    printf( "direction %d\n", (int)packet.direction );
    printf( "timestamp %u\n", (unsigned int)packet.timestamp );
    printf( "timestamp_reply %u\n", (unsigned int)packet.timestamp_reply );

    Network::Fragment fragment( packet.payload );
    printf( "frag_id %llu\n", (unsigned long long)fragment.id );
    printf( "frag_num %u\n", (unsigned int)fragment.fragment_num );
    printf( "frag_final %d\n", (int)fragment.final );
    printf( "frag_contents_len %zu\n", fragment.contents.size() );

    /* A single-fragment instruction can be parsed here and its fields checked. */
    if ( fragment.final && fragment.fragment_num == 0 ) {
      Network::FragmentAssembly assembly;
      if ( assembly.add_fragment( fragment ) ) {
        TransportBuffers::Instruction inst = assembly.get_assembly();
        printf( "protocol_version %u\n", inst.protocol_version() );
        printf( "old_num %llu\n", (unsigned long long)inst.old_num() );
        printf( "new_num %llu\n", (unsigned long long)inst.new_num() );
        printf( "ack_num %llu\n", (unsigned long long)inst.ack_num() );
        printf( "throwaway_num %llu\n", (unsigned long long)inst.throwaway_num() );
        printf( "diff %s\n", inst.diff().c_str() );
      }
    }
  } catch ( const Crypto::CryptoException& e ) {
    fprintf( stderr, "CryptoException: %s\n", e.what() );
    return 1;
  }

  return 0;
}
