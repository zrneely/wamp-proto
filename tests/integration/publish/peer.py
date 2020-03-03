#!/usr/bin/env python3

import sys
from autobahn.asyncio.component import Component, run


async def publish_one_message_on_joined(session, details):
    # We expect another client to publish a single message to 'org.test.topic1'.
    def received_msg(event):
        print(event)
        print('test passed: received message')

    try:
        await session.subscribe(received_msg, 'org.test.topic1')
        print('ready: subscribed to org.test.topic1')

    except Exception as e:
        print('test failed: unable to subscribe to topic:', e)


def run_test_peer(component, test_name):
    print('running test peer:', test_name)

    # pylint: disable=unused-variable
    @component.on_join
    async def joined(session, details):
        print('test component joined:', test_name, session, details)

        if test_name == 'publishOneMessage':
            await publish_one_message_on_joined(session, details)

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
