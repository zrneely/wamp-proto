#!/usr/bin/env python3

import sys
from autobahn.asyncio.component import Component, run


async def publish_forever(session, details):
    try:
        session.publish('org.test.channel', {
            'a': 1,
            'b': False,
            'c': [3, 4, 5]
        })
        print('ready: published to org.test.channel')
        print('test passed: peer has no validation to perform')

    except Exception as e:
        print('test failed: unable to publish to topic:', e)


def run_test_peer(component, test_name):
    print('running test peer:', test_name)

    # pylint: disable=unused-variable
    @component.on_join
    async def joined(session, details):
        print('test component joined:', test_name, session, details)

        if test_name == 'subscribeRecvUnsubscribe':
            await publish_forever(session, details)

        else:
            print('test failed: unknown test name')

    run([component])


if __name__ == '__main__':
    test_name = sys.argv[1]
    router_url = sys.argv[2]
    test_realm = sys.argv[3]

    component = Component(
        transports=router_url,
        realm=test_realm,
    )
    run_test_peer(component, test_name)
