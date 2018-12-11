'use strict';

// Peer clients to the publish integration tests.
// Usage: node peer.js <test name> <router url> <test realm>

const autobahn = require('autobahn');

function publishOneMessageTestPeer(routerUrl, testRealm) {
    const connection = new autobahn.Connection({'url': routerUrl, 'realm': testRealm});
    connection.onopen = (session) => {
        session.subscribe('org.test.topic1', (args) => {
            // This test expects to receive one publication to this channel.
            if (args[0] === 42) {
                console.log('test passed');
            } else {
                console.log('test failed');
            }
        });

        // We've subscribed to all channels; tell the test to proceed.
        console.log('ready');
    };
    connection.open();
    console.error("opening connection");
}

function publishMultipleMessagesTestPeer(routerUrl, testRealm) {
    const connection = new autobahn.Connection({'url': routerUrl, 'realm': testRealm});
    connection.onopen = (session) => {
        let recvTopic1 = false;
        let recvTopic2Msg1 = false;
        let recvTopic2Msg2 = false;

        session.subscribe('org.test.topic1', (args, kwargs) => {

        });

        session.subscribe('org.test.topic2', (args, kwargs) => {

        });

        // We've subscribed to all channels; tell the test to proceed.
        console.log('ready');
    };
    connection.open();
}


const testName = process.argv[2];
const routerUrl = process.argv[3];
const testRealm = process.argv[4];
console.error("starting peer:", testName, routerUrl, testRealm);

switch (testName) {
    case "publishOneMessage": {
        publishOneMessageTestPeer(routerUrl, testRealm);
        break;
    }
    case "publishMultipleMessages": {
        publishMultipleMessagesTestPeer(routerUrl, testRealm);
        break;
    }
}